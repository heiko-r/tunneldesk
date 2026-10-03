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

/** State of the `cloudflared` connector as reported by the core. */
export type ConnectorState = "Stopped" | "Starting" | "Connected" | "External" | "NotInstalled";

export type CloudflareStatus = {
  configured: boolean;
  tunnelId?: string;
  tunnelName?: string;
  connector: ConnectorState;
};

/** Status of the shared core process this UI is attached to. */
export type CoreStatus = {
  attachedClients: number;
  pid: number;
  port: number;
  configPath: string;
  /** Seconds of inactivity before the core exits; `null` when it runs until stopped. */
  idleTimeoutSecs: number | null;
  /** Set once the core announced that it is shutting down. */
  shuttingDown: boolean;
};

export type SyncReport = {
  added: string[];
  removed: string[];
  unknownHosts: string[];
  errors: string[];
};
