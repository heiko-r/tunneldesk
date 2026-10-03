//! Transport-level protection for the core's HTTP endpoint.
//!
//! Anything on the machine can reach `127.0.0.1:<port>`, including web pages
//! open in the user's browser (WebSockets are not covered by CORS) and DNS
//! rebinding attacks. Every request therefore has to pass a Host check, an
//! Origin check and must carry the access token from the config file.

use axum::http::{HeaderMap, Method, StatusCode, Uri, header};

/// Path that stays reachable without a token so frontends can find the core.
pub const HEALTH_PATH: &str = "/api/health";

/// Result of evaluating a request against the [`SecurityPolicy`].
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// Forward the request; `authenticated` is false only for the health check.
    Allow {
        authenticated: bool,
    },
    /// Exchange a `?token=` URL for a cookie, then let the page navigate itself.
    ///
    /// This must not be an HTTP redirect. The native window reaches `/?token=`
    /// from a splash page on another origin, and browsers withhold a cookie
    /// just set with `SameSite=Strict` from the redirect that follows, so the
    /// next request arrives without credentials.
    SetCookieAndEnter {
        cookie: String,
        location: String,
    },
    /// Serve the "how to open the UI" page (unauthenticated browser visit).
    LoginPage,
    Reject(StatusCode, &'static str),
}

/// Host, Origin and token rules for one core.
#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    port: u16,
    token: String,
    allowed_origins: Vec<String>,
}

impl SecurityPolicy {
    pub fn new(port: u16, token: impl Into<String>, extra_origins: &[String]) -> Self {
        let mut allowed_origins: Vec<String> = local_authorities(port)
            .into_iter()
            .map(|authority| format!("http://{authority}"))
            .collect();
        allowed_origins.extend(
            extra_origins
                .iter()
                .map(|o| o.trim_end_matches('/').to_string()),
        );
        Self {
            port,
            token: token.into(),
            allowed_origins,
        }
    }

    /// Name of the auth cookie. Cookies ignore ports, so the port is part of
    /// the name to keep cores for different config files apart.
    pub fn cookie_name(&self) -> String {
        format!("tunneldesk_token_{}", self.port)
    }

    pub fn evaluate(&self, method: &Method, uri: &Uri, headers: &HeaderMap) -> Decision {
        let host = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        if !local_authorities(self.port).iter().any(|a| a == host) {
            return Decision::Reject(StatusCode::FORBIDDEN, "Forbidden host");
        }

        if uri.path() == HEALTH_PATH && (method == Method::GET || method == Method::HEAD) {
            return Decision::Allow {
                authenticated: false,
            };
        }

        if let Some(origin) = headers.get(header::ORIGIN) {
            let origin = origin.to_str().unwrap_or("");
            if !self.allowed_origins.iter().any(|o| o == origin) {
                return Decision::Reject(StatusCode::FORBIDDEN, "Forbidden origin");
            }
        }

        if self.bearer_matches(headers) || self.cookie_matches(headers) {
            return Decision::Allow {
                authenticated: true,
            };
        }

        let query_token_allowed = matches!(uri.path(), "/" | "/ws");
        if query_token_allowed && let Some(candidate) = query_token(uri) {
            if !constant_time_eq(candidate.as_bytes(), self.token.as_bytes()) {
                return Decision::Reject(StatusCode::UNAUTHORIZED, "Invalid token");
            }
            if uri.path() == "/" && method == Method::GET {
                return Decision::SetCookieAndEnter {
                    cookie: format!(
                        "{}={}; HttpOnly; SameSite=Strict; Path=/",
                        self.cookie_name(),
                        self.token
                    ),
                    location: "/".to_string(),
                };
            }
            return Decision::Allow {
                authenticated: true,
            };
        }

        if method == Method::GET && uri.path() == "/" {
            Decision::LoginPage
        } else {
            Decision::Reject(StatusCode::UNAUTHORIZED, "Unauthorized")
        }
    }

    fn bearer_matches(&self, headers: &HeaderMap) -> bool {
        headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .is_some_and(|t| constant_time_eq(t.trim().as_bytes(), self.token.as_bytes()))
    }

    fn cookie_matches(&self, headers: &HeaderMap) -> bool {
        let name = self.cookie_name();
        headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|h| h.to_str().ok())
            .flat_map(|h| h.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .any(|(k, v)| k == name && constant_time_eq(v.as_bytes(), self.token.as_bytes()))
    }
}

/// `host:port` values under which the core is legitimately addressed.
fn local_authorities(port: u16) -> [String; 3] {
    [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]
}

fn query_token(uri: &Uri) -> Option<&str> {
    uri.query()?
        .split('&')
        .find_map(|pair| pair.strip_prefix("token="))
}

/// Compares secrets without leaking the position of the first mismatch.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Page shown when the UI is opened without a token.
pub const LOGIN_PAGE: &str = r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>TunnelDesk</title>
<style>body{background:#080b0f;color:#c8d8c0;font-family:monospace;display:flex;
align-items:center;justify-content:center;height:100vh;margin:0}
code{color:#3ddc84}</style></head>
<body><div><p>This TunnelDesk instance requires an access token.</p>
<p>Open it with <code>tunneldesk open</code>, or from the desktop app.</p></div></body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const PORT: u16 = 4567;
    const TOKEN: &str = "secret-token";

    fn policy() -> SecurityPolicy {
        SecurityPolicy::new(PORT, TOKEN, &["http://localhost:5173/".to_string()])
    }

    fn headers(pairs: &[(header::HeaderName, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.append(k.clone(), HeaderValue::from_str(v).unwrap());
        }
        map
    }

    fn host() -> (header::HeaderName, &'static str) {
        (header::HOST, "127.0.0.1:4567")
    }

    fn eval(method: Method, uri: &str, pairs: &[(header::HeaderName, &str)]) -> Decision {
        policy().evaluate(&method, &uri.parse().unwrap(), &headers(pairs))
    }

    #[test]
    fn rejects_foreign_host() {
        let d = eval(
            Method::GET,
            "/api/health",
            &[(header::HOST, "evil.example.com:4567")],
        );
        assert_eq!(d, Decision::Reject(StatusCode::FORBIDDEN, "Forbidden host"));
    }

    #[test]
    fn rejects_missing_host_and_wrong_port() {
        assert!(matches!(
            eval(Method::GET, "/", &[]),
            Decision::Reject(StatusCode::FORBIDDEN, _)
        ));
        assert!(matches!(
            eval(Method::GET, "/", &[(header::HOST, "127.0.0.1:1")]),
            Decision::Reject(StatusCode::FORBIDDEN, _)
        ));
    }

    #[test]
    fn accepts_all_local_host_spellings() {
        for h in ["127.0.0.1:4567", "localhost:4567", "[::1]:4567"] {
            let d = eval(
                Method::GET,
                "/ws",
                &[
                    (header::HOST, h),
                    (header::AUTHORIZATION, "Bearer secret-token"),
                ],
            );
            assert_eq!(
                d,
                Decision::Allow {
                    authenticated: true
                },
                "host {h}"
            );
        }
    }

    #[test]
    fn health_needs_no_token_and_ignores_origin() {
        let d = eval(
            Method::GET,
            "/api/health",
            &[host(), (header::ORIGIN, "null")],
        );
        assert_eq!(
            d,
            Decision::Allow {
                authenticated: false
            }
        );
    }

    #[test]
    fn rejects_foreign_origin_even_with_token() {
        let d = eval(
            Method::GET,
            "/ws",
            &[
                host(),
                (header::ORIGIN, "https://evil.example.com"),
                (header::AUTHORIZATION, "Bearer secret-token"),
            ],
        );
        assert_eq!(
            d,
            Decision::Reject(StatusCode::FORBIDDEN, "Forbidden origin")
        );
    }

    #[test]
    fn accepts_own_and_configured_origins() {
        for origin in ["http://127.0.0.1:4567", "http://localhost:5173"] {
            let d = eval(
                Method::POST,
                "/mcp",
                &[
                    host(),
                    (header::ORIGIN, origin),
                    (header::AUTHORIZATION, "Bearer secret-token"),
                ],
            );
            assert_eq!(
                d,
                Decision::Allow {
                    authenticated: true
                },
                "origin {origin}"
            );
        }
    }

    #[test]
    fn missing_token_is_unauthorized() {
        assert_eq!(
            eval(Method::POST, "/mcp", &[host()]),
            Decision::Reject(StatusCode::UNAUTHORIZED, "Unauthorized")
        );
        assert_eq!(eval(Method::GET, "/", &[host()]), Decision::LoginPage);
    }

    #[test]
    fn wrong_bearer_token_is_unauthorized() {
        let d = eval(
            Method::POST,
            "/mcp",
            &[host(), (header::AUTHORIZATION, "Bearer nope")],
        );
        assert_eq!(
            d,
            Decision::Reject(StatusCode::UNAUTHORIZED, "Unauthorized")
        );
    }

    #[test]
    fn cookie_token_is_accepted() {
        let d = eval(
            Method::GET,
            "/_app/app.js",
            &[
                host(),
                (
                    header::COOKIE,
                    "other=1; tunneldesk_token_4567=secret-token",
                ),
            ],
        );
        assert_eq!(
            d,
            Decision::Allow {
                authenticated: true
            }
        );
    }

    #[test]
    fn cookie_for_another_port_is_ignored() {
        let d = eval(
            Method::GET,
            "/_app/app.js",
            &[
                host(),
                (header::COOKIE, "tunneldesk_token_1234=secret-token"),
            ],
        );
        assert_eq!(
            d,
            Decision::Reject(StatusCode::UNAUTHORIZED, "Unauthorized")
        );
    }

    #[test]
    fn query_token_on_root_sets_cookie_and_redirects() {
        let d = eval(Method::GET, "/?token=secret-token", &[host()]);
        assert_eq!(
            d,
            Decision::SetCookieAndEnter {
                cookie: "tunneldesk_token_4567=secret-token; HttpOnly; SameSite=Strict; Path=/"
                    .to_string(),
                location: "/".to_string(),
            }
        );
    }

    #[test]
    fn query_token_on_ws_is_accepted() {
        let d = eval(Method::GET, "/ws?token=secret-token", &[host()]);
        assert_eq!(
            d,
            Decision::Allow {
                authenticated: true
            }
        );
    }

    #[test]
    fn wrong_query_token_is_rejected() {
        let d = eval(Method::GET, "/?token=nope", &[host()]);
        assert_eq!(
            d,
            Decision::Reject(StatusCode::UNAUTHORIZED, "Invalid token")
        );
    }

    #[test]
    fn query_token_is_ignored_on_other_paths() {
        let d = eval(Method::POST, "/api/shutdown?token=secret-token", &[host()]);
        assert_eq!(
            d,
            Decision::Reject(StatusCode::UNAUTHORIZED, "Unauthorized")
        );
    }

    #[test]
    fn constant_time_eq_compares_content_and_length() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
