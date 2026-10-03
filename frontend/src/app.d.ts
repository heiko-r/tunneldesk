// See https://svelte.dev/docs/kit/types#app.d.ts
// for information about these interfaces
declare global {
  namespace App {
    // interface Error {}
    // interface Locals {}
    // interface PageData {}
    // interface PageState {}
    // interface Platform {}
  }

  interface ImportMetaEnv {
    readonly VITE_BACKEND_PORT?: string;
    /** `[gui] access_token` of the dev core; put it in `.env.development.local`. */
    readonly VITE_BACKEND_TOKEN?: string;
  }

  interface Window {
    /** IPC channel injected by the native (wry) window; absent in a browser tab. */
    ipc?: { postMessage(message: string): void };
  }
}

export {};
