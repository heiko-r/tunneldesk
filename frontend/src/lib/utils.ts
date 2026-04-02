/** Encodes a string to base64 using UTF-8. */
export function encodeBase64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}

/** Decodes a base64 string to a Uint8Array. Returns an empty array on failure. */
export function decodeBase64(base64: string): Uint8Array {
  try {
    const binaryString = atob(base64);
    const bytes = new Uint8Array(binaryString.length);
    for (let i = 0; i < binaryString.length; i++) {
      bytes[i] = binaryString.charCodeAt(i);
    }
    return bytes;
  } catch {
    return new Uint8Array();
  }
}

/** Converts a Uint8Array to a space-separated hex string. */
export function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join(" ");
}

/** Decodes a Uint8Array to a UTF-8 string (lossy). */
export function bytesToUtf(bytes: Uint8Array): string {
  try {
    return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  } catch {
    return "[UTF-8 decode error]";
  }
}

/** Pretty-prints a JSON string. Returns the original string on parse failure. */
export function formatJson(text: string): string {
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

/**
 * Adds basic indentation to an XML string.
 * Uses a simple regex-based approach; not a full XML parser.
 */
export function formatXml(text: string): string {
  try {
    const PADDING = " ".repeat(2);
    let pad = 0;
    text = text.replace(/(>)(<)(\/*)/g, "$1\n$2$3");
    return text
      .split("\n")
      .map((node) => {
        let indent = 0;
        if (node.match(/^.+<\/\w[^>]*>$/)) {
          indent = 0;
        } else if (node.match(/^<\/\w/) && pad > 0) {
          pad -= 1;
        } else if (node.match(/^<\w[^>]*[^/]>.*$/)) {
          indent = 1;
        }
        const padding = PADDING.repeat(pad);
        pad += indent;
        return padding + node;
      })
      .join("\n");
  } catch {
    return text;
  }
}

export function methodClass(m: string): string {
  return `method-${m.toLowerCase()}`;
}
export function statusClass(s: number | undefined): string {
  if (s == null) return "status-pending";
  if (s < 200) return "status-1xx";
  if (s < 300) return "status-2xx";
  if (s < 400) return "status-3xx";
  if (s < 500) return "status-4xx";
  return "status-5xx";
}

export function fmtTime(d: Date): string {
  return d.toLocaleTimeString("en-US", {
    hour12: false,
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}
export function fmtMs(n: number | undefined): string {
  if (n == null) {
    return "—";
  }
  if (n >= 1000) {
    return `${(n / 1000).toFixed(2)}s`;
  } else if (n >= 100) {
    return `${n.toFixed(0)}ms`;
  } else if (n >= 10) {
    return `${n.toFixed(1)}ms`;
  } else {
    return `${n.toFixed(3)}ms`;
  }
}
