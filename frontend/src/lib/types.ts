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
  wsMessages: {
    dir: "in" | "out";
    ts: Date;
    data: string;
  }[];
};

export type RequestTab = "headers" | "request" | "response" | "ws";
