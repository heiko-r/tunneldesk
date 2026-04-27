use std::io::Cursor;
use std::sync::Arc;

use image::ImageReader;
use rust_embed::Embed;
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::{Icon, WindowBuilder},
};
use wry::WebViewBuilder;

use crate::{Args, config::Config};

#[derive(Embed)]
#[folder = "assets"]
struct AppAssets;

/// Launches the native GUI window. Runs the tao event loop on the calling
/// (main) thread; the Tokio runtime and all app logic run on a background thread.
///
/// This function never returns.
pub fn launch(args: Args) -> ! {
    // Read the port from config before building the WebView so that the loading
    // page can start polling the server immediately, without any IPC round-trip.
    // This avoids a race where `load_url` is called before the WebView has
    // finished initialising its initial page, which caused an occasional blank
    // white screen on first launch.
    let config_path = args.resolved_config();
    let port = if config_path.exists() {
        Config::from_file(&config_path)
            .map(|c| c.gui.port)
            .unwrap_or_else(|_| Config::default_config().gui.port)
    } else {
        Config::default_config().gui.port
    };

    let event_loop = EventLoopBuilder::<()>::with_user_event().build();

    // Channel to signal the background thread to shut down when the window closes.
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::sync_channel::<()>(1);
    // Channel for background thread to signal when cleanup is complete.
    let (cleanup_done_tx, cleanup_done_rx) = std::sync::mpsc::sync_channel::<()>(1);

    // Background thread: runs the Tokio runtime and all async app logic.
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("failed to create Tokio runtime");
        rt.block_on(async move {
            match crate::init_app(&args).await {
                Ok((tunnel_manager, web_server_handle, tunnel_sync, config)) => {
                    // Wait for the window-close signal from the main thread.
                    tokio::task::spawn_blocking(move || shutdown_rx.recv().ok())
                        .await
                        .ok();

                    // Cloudflare cleanup: remove all configured tunnels on shutdown
                    if let Some(sync) = tunnel_sync {
                        let cfg = config.read().await;
                        if let Err(e) = sync.remove_all_configured_tunnels(&cfg).await {
                            tracing::warn!(
                                "Failed to remove tunnels from Cloudflare during shutdown: {e}"
                            );
                        } else {
                            tracing::info!("Removed all configured tunnels from Cloudflare");
                        }
                        drop(cfg);
                    }

                    tunnel_manager.shutdown().await;
                    web_server_handle.abort();

                    // Signal main thread that cleanup is complete.
                    let _ = cleanup_done_tx.send(());
                }
                Err(e) => {
                    tracing::error!("App initialisation failed: {e}");
                }
            }
        });
    });

    let icon_file = AppAssets::get("icon.png").expect("failed to load app icon");
    let mut icon_reader = ImageReader::new(Cursor::new(icon_file.data));
    icon_reader.set_format(image::ImageFormat::Png);
    let icon_data = icon_reader
        .decode()
        .expect("failed to decode app icon")
        .to_rgba8();
    let icon = Icon::from_rgba(icon_data.into_raw(), 256, 256).expect("failed to create app icon");

    let window = WindowBuilder::new()
        .with_title("TunnelDesk")
        .with_inner_size(LogicalSize::new(1280.0_f64, 800.0_f64))
        .with_window_icon(Some(icon))
        .build(&event_loop)
        .expect("failed to create window");

    let loading_html = make_loading_html(port);
    let webview = build_webview(&window, &loading_html).expect("failed to create webview");

    // Keep shutdown_tx and cleanup_done_rx alive in the closure so they are
    // dropped (closing the channel) only when the event loop exits.
    let shutdown_tx = Arc::new(std::sync::Mutex::new(Some(shutdown_tx)));
    let cleanup_done_rx = Arc::new(std::sync::Mutex::new(Some(cleanup_done_rx)));

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        // Keep the webview alive for the duration of the event loop.
        let _ = &webview;

        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            // Signal the background thread to start shutdown.
            if let Ok(mut guard) = shutdown_tx.lock()
                && let Some(tx) = guard.take()
            {
                tx.send(()).ok();
            }

            // Wait for background thread to complete cleanup before exiting.
            if let Ok(mut guard) = cleanup_done_rx.lock()
                && let Some(rx) = guard.take()
            {
                // Use a timeout to avoid hanging if cleanup fails
                let _ = rx.recv_timeout(std::time::Duration::from_secs(10));
            }

            *control_flow = ControlFlow::Exit;
        }
    })
}

/// Builds the splash-screen HTML with an embedded JS fetch-poll that
/// navigates to the app once the local server responds.  Using JS polling
/// instead of a Rust `load_url` call avoids a race where `load_url` is
/// issued before the WebView has finished loading its initial document,
/// which produced an occasional blank white screen on first launch.
fn make_loading_html(port: u16) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<style>
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  html, body {{
    width: 100%; height: 100%;
    background: #080b0f;
    display: flex; align-items: center; justify-content: center;
  }}
  svg {{
    width: 96px; height: 96px;
    opacity: 0;
    animation: fadein 0.4s ease-out 0.1s forwards;
  }}
  @keyframes fadein {{ to {{ opacity: 1; }} }}
</style>
</head>
<body>
<script>
(function () {{
  var url = 'http://127.0.0.1:{port}';
  var id = setInterval(function () {{
    fetch(url, {{ method: 'HEAD', cache: 'no-store', mode: 'no-cors' }})
      .then(function () {{ clearInterval(id); window.location.replace(url); }})
      .catch(function () {{}});
  }}, 150);
}})();
</script>
<svg width="83.34375mm" height="83.34375mm" viewBox="0 0 83.34375 83.34375" xmlns="http://www.w3.org/2000/svg">
  <g transform="translate(-51.46146,-30.030208)">
    <path style="fill:#3ddc84" d="M 153.5,309.62711 C 151.31204,308.84555 54.336096,255.55001 37.004837,245.6043 26.309833,239.46687 25.386355,238.79373 24.721667,236.6508 24.293361,235.26996 23.992116,216.96935 23.620419,169.75 23.071209,99.98004 23.337206,81.139975 24.913823,78.140275 26.486793,75.147515 32.878958,71.397015 90.75,39.511947 152.57577,5.4479483 152.94522,5.2625102 159,5.2545826 l 3.5,-0.00458 12.75,6.9610884 c 10.6771,5.829352 47.70585,26.25099 98.99869,54.598532 12.53122,6.92551 15.42253,9.28923 16.70423,13.65613 0.98305,3.34938 1.03538,9.49604 0.0999,11.734965 -2.02126,4.837565 -7.17996,4.22252 -9.06759,-1.08109 C 281.52136,89.81633 280.89126,88.07665 280.585,87.25367 279.2396,83.6383 274.18398,79.74086 261,72.15536 257.15,69.940235 237.9125,59.286305 218.25,48.479964 161.4649,17.271363 159.24221,16.141846 156.28376,16.990318 154.48634,17.505811 142.86956,23.485012 131.5,29.746615 42.707359,78.64774 38.003647,81.337275 35.607977,84.576735 c -0.851271,1.1511 -1.036193,2.574185 -1.396902,10.75 -0.573962,13.009365 -0.540768,117.434845 0.03985,125.373325 0.751891,10.28018 2.031914,13.10332 6.858077,15.12573 C 42.814589,236.54052 57.536011,244.54931 103,269.49591 155.70839,298.41758 154.9053,298 157.81841,298 c 3.07642,0 8.76503,-3.01129 70.18159,-37.15092 43.59232,-24.23167 50.35845,-28.26406 51.51497,-30.70126 1.21724,-2.56512 1.64173,-12.34194 2.01867,-46.49269 l 0.34874,-31.59486 1.30881,-1.22397 c 0.71985,-0.67318 1.80381,-1.37333 2.40882,-1.55589 1.66574,-0.50265 3.98149,0.93116 4.58028,2.83589 1.25744,3.99989 2.15565,66.89822 1.09872,76.93955 -0.65122,6.18682 -1.34197,8.29047 -3.43843,10.47149 -1.27147,1.32275 -7.87005,5.22715 -21.34058,12.6273 -10.725,5.89185 -35.5875,19.6742 -55.25,30.62744 -41.77857,23.27324 -43.76348,24.35127 -47.85095,25.98843 -4.28758,1.71731 -6.86514,1.94035 -9.89905,0.8566 z" transform="matrix(0.26458333,0,0,0.26458333,51.46146,30.030208)"/>
    <path style="fill:#3ddc84" d="m 42.97835,193.64613 c -0.217171,-0.49287 -0.493368,-4.30282 -0.613771,-8.46655 l -0.218915,-7.57041 5.12514,-6.92959 C 66.425782,144.78058 79.832267,133.31701 109,117.89635 c 12.39254,-6.55179 14.96851,-8.12959 18.35372,-11.2417 10.64254,-9.783995 34.44065,-20.138488 50.88316,-22.139128 15.98334,-1.94477 27.77047,0.508801 58.26312,12.127863 17.6189,6.713595 28.61423,9.724215 41.39806,11.335205 8.86605,1.11727 10.90145,1.57854 13.1997,2.99137 2.18622,1.34396 3.0566,4.20822 2.17425,7.15504 -1.96004,6.54595 -13.47854,19.17506 -26.96103,29.56058 -18.05665,13.90898 -57.16197,34.7955 -70.28907,37.54206 -4.26829,0.89303 -5.77172,0.4825 -6.94853,-1.89739 -0.79535,-1.6085 -0.83734,-2.08945 -0.29638,-3.39545 0.83822,-2.02364 2.91995,-3.25032 10.973,-6.46599 17.44833,-6.96728 47.59991,-23.9202 59.5,-33.45425 8.63921,-6.92153 21.5,-19.47653 21.5,-20.98881 0,-0.57663 -1.2974,-0.84792 -8.5,-1.77736 -11.99662,-1.54807 -20.7888,-3.90439 -35.5,-9.51409 C 208.66573,97.025162 203.63903,95.81697 187,95.776653 c -8.94051,-0.02166 -11.79571,0.147473 -15.25,0.903376 -12.52335,2.74049 -27.31065,9.761991 -37.0588,17.596761 -5.16729,4.15303 -11.0041,7.74639 -19.71822,12.13924 -22.95908,11.57383 -41.433223,26.6474 -56.083377,45.76 -2.082371,2.71666 -6.203018,8.78629 -9.156994,13.48805 -2.953976,4.70177 -5.593286,8.62281 -5.865133,8.71343 -0.271848,0.0906 -0.671954,-0.23851 -0.889126,-0.73138 z" transform="matrix(0.26458333,0,0,0.26458333,51.46146,30.030208)"/>
    <path style="fill:#3ddc84" d="m 210.5,124.04744 c -1.90377,-0.8444 -3.82264,-3.37958 -4.09502,-5.41028 -0.71724,-5.34744 5.35322,-9.03138 9.89608,-6.00556 3.63828,2.42331 4.30992,6.82942 1.51744,9.95475 -1.11832,1.25162 -1.84325,1.57627 -3.8482,1.72338 -1.35866,0.0997 -2.9203,-0.0183 -3.4703,-0.26229 z" transform="matrix(0.26458333,0,0,0.26458333,51.46146,30.030208)"/>
    <path style="fill:#3ddc84" d="m 59.225794,230.52977 c -10.788965,-0.30882 -11.68692,-0.55392 -13.274127,-3.62324 -0.762741,-1.47497 -0.760802,-1.62671 0.02817,-2.20362 0.466023,-0.34076 2.798705,-0.78274 5.183738,-0.98217 5.451523,-0.45585 36.088186,0.0682 45.086424,0.77124 10.538741,0.82337 21.304771,1.97671 24.152031,2.58734 2.20102,0.47203 2.42722,0.61929 1.48602,0.96746 -4.69654,1.73737 -39.932407,3.13358 -62.662257,2.48299 z" transform="matrix(0.26458333,0,0,0.26458333,51.46146,30.030208)"/>
    <path style="fill:#3ddc84" d="m 150.93,232.3368 c -1.6115,-1.18977 -3.23691,-2.53749 -3.61203,-2.99495 -1.35203,-1.64882 -7.67524,-5.99595 -12.39654,-8.52247 -5.79472,-3.10095 -7.81264,-5.26207 -7.87771,-8.43678 -0.0405,-1.97342 0.0601,-2.14453 1.34755,-2.29238 1.82044,-0.20907 5.64974,1.0714 10.60873,3.54743 10.10219,5.04403 17.29513,12.19473 17.42971,17.32736 0.0513,1.95648 -0.81317,3.53499 -1.93593,3.53499 -0.34857,0 -1.95227,-0.97344 -3.56378,-2.1632 z" transform="matrix(0.26458333,0,0,0.26458333,51.46146,30.030208)"/>
    <path style="fill:#3ddc84" d="m 180.6022,237.94719 c -0.49378,-0.28732 -2.79029,-3.40144 -5.10333,-6.92026 -4.28179,-6.51387 -9.75034,-12.60775 -13.81777,-15.39784 -3.12303,-2.14227 -7.43731,-4.22738 -13.1811,-6.37048 -5.93213,-2.21337 -7.61979,-3.02793 -9.125,-4.40425 -1.45743,-1.33264 -1.42203,-2.401 0.12779,-3.85698 1.6931,-1.59058 7.91155,-1.96314 12.99721,-0.77868 12.19083,2.83926 26.28632,14.4401 32.47065,26.72393 2.46537,4.89693 3.05596,7.18248 2.31694,8.96662 -0.94555,2.28276 -4.43601,3.34678 -6.68539,2.03794 z" transform="matrix(0.26458333,0,0,0.26458333,51.46146,30.030208)"/>
    <path style="fill:#3ddc84" d="m 210.16378,235.98165 c -0.45992,-0.26837 -1.90497,-2.45527 -3.21123,-4.85979 -3.1379,-5.77621 -5.50623,-8.93569 -10.82048,-14.43513 -5.61183,-5.8074 -8.67733,-8.02065 -17.74215,-12.80963 -7.05377,-3.72654 -10.13992,-5.90366 -10.13992,-7.15322 0,-0.32396 0.46633,-1.18186 1.03629,-1.90645 0.93859,-1.19323 1.35786,-1.31743 4.44733,-1.31743 11.45841,0 28.27198,11.15047 37.2189,24.68293 3.61795,5.47225 6.25427,10.86222 6.62078,13.53623 0.26275,1.91698 0.13514,2.36785 -0.98806,3.49106 -0.99352,0.99352 -1.78312,1.28629 -3.43751,1.27458 -1.18125,-0.008 -2.52403,-0.23478 -2.98395,-0.50315 z" transform="matrix(0.26458333,0,0,0.26458333,51.46146,30.030208)"/>
  </g>
</svg>
</body>
</html>"#,
        port = port
    )
}

/// Constructs the [`WebView`] for the given window, handling the platform
/// difference between Unix (GTK container) and other platforms.
#[cfg(not(target_os = "linux"))]
fn build_webview(window: &tao::window::Window, html: &str) -> wry::Result<wry::WebView> {
    WebViewBuilder::new().with_html(html).build(window)
}

#[cfg(target_os = "linux")]
fn build_webview(window: &tao::window::Window, html: &str) -> wry::Result<wry::WebView> {
    use tao::platform::unix::WindowExtUnix;
    use wry::WebViewBuilderExtUnix;

    let vbox = window.default_vbox().expect("no GTK vbox on window");
    WebViewBuilder::new().with_html(html).build_gtk(vbox)
}
