export type HttpMethod =
  | "GET"
  | "POST"
  | "PUT"
  | "DELETE"
  | "PATCH"
  | "OPTIONS"
  | "HEAD"
  | "CONNECT"
  | "TRACE";

export type Tunnel = {
  name: string;
  domain: string;
  localPort: number;
  active: boolean;
  /** Whether Cloudflare routing is enabled for this tunnel. */
  enabled: boolean;
  socketPath: string;
};

export type TunneledRequest = {
  id: string;
  tunnelName: string;
  timestamp: Date;
  method: HttpMethod;
  url: string;
  status?: number;
  responseTime?: number;
  requestHeaders: { [key: string]: string };
  responseHeaders?: { [key: string]: string };
  requestBody: string | null;
  responseBody?: string;
  isWebSocket?: boolean;
  /** `true` when this request was created by the replay feature. */
  replayed?: boolean;
  wsMessages: {
    dir: "in" | "out";
    ts: Date;
    data: string;
  }[];
};

export type RequestTab = "headers" | "request" | "response" | "ws";

export type CloudflareStatus = {
  configured: boolean;
  tunnelId?: string;
  tunnelName?: string;
  serviceRunning: boolean;
};

export type SyncReport = {
  added: string[];
  removed: string[];
  unknownHosts: string[];
  errors: string[];
};
