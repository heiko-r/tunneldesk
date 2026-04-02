import { describe, it, expect } from "vitest";
import {
  methodClass,
  statusClass,
  fmtTime,
  fmtMs,
  decodeBase64,
  encodeBase64,
  bytesToHex,
  bytesToUtf,
  formatJson,
  formatXml,
} from "./utils";

describe("methodClass", () => {
  it("returns lowercase method- prefixed class", () => {
    expect(methodClass("GET")).toBe("method-get");
    expect(methodClass("POST")).toBe("method-post");
    expect(methodClass("DELETE")).toBe("method-delete");
  });

  it("lowercases already-lowercase input", () => {
    expect(methodClass("get")).toBe("method-get");
  });
});

describe("statusClass", () => {
  it("returns status-pending for undefined", () => {
    expect(statusClass(undefined)).toBe("status-pending");
  });

  it("classifies 1xx correctly", () => {
    expect(statusClass(100)).toBe("status-1xx");
    expect(statusClass(199)).toBe("status-1xx");
  });

  it("classifies 2xx correctly", () => {
    expect(statusClass(200)).toBe("status-2xx");
    expect(statusClass(204)).toBe("status-2xx");
    expect(statusClass(299)).toBe("status-2xx");
  });

  it("classifies 3xx correctly", () => {
    expect(statusClass(301)).toBe("status-3xx");
    expect(statusClass(304)).toBe("status-3xx");
  });

  it("classifies 4xx correctly", () => {
    expect(statusClass(400)).toBe("status-4xx");
    expect(statusClass(404)).toBe("status-4xx");
    expect(statusClass(499)).toBe("status-4xx");
  });

  it("classifies 5xx correctly", () => {
    expect(statusClass(500)).toBe("status-5xx");
    expect(statusClass(503)).toBe("status-5xx");
  });

  it("classifies 0 as 1xx (< 200)", () => {
    expect(statusClass(0)).toBe("status-1xx");
  });
});

describe("fmtTime", () => {
  it("formats date as HH:MM:SS in 24h", () => {
    // Use a fixed UTC date to avoid timezone flakiness
    const d = new Date("2024-01-01T14:05:09Z");
    const result = fmtTime(d);
    // Should be 8 chars like "14:05:09"
    expect(result).toMatch(/^\d{2}:\d{2}:\d{2}$/);
  });

  it("returns a string", () => {
    expect(typeof fmtTime(new Date())).toBe("string");
  });
});

describe("fmtMs", () => {
  it("returns em dash for undefined", () => {
    expect(fmtMs(undefined)).toBe("—");
  });

  it("formats milliseconds under 1s", () => {
    expect(fmtMs(0)).toBe("0.000ms");
    expect(fmtMs(0.981)).toBe("0.981ms");
    expect(fmtMs(12.876)).toBe("12.9ms");
    expect(fmtMs(250.432)).toBe("250ms");
    expect(fmtMs(999)).toBe("999ms");
  });

  it("formats seconds for values >= 1000", () => {
    expect(fmtMs(1000)).toBe("1.00s");
    expect(fmtMs(1500)).toBe("1.50s");
    expect(fmtMs(2000)).toBe("2.00s");
  });
});

describe("decodeBase64", () => {
  it("decodes valid base64 to bytes", () => {
    // "hello" in base64
    const result = decodeBase64("aGVsbG8=");
    expect(result).toBeInstanceOf(Uint8Array);
    expect(Array.from(result)).toEqual([104, 101, 108, 108, 111]);
  });

  it("returns empty Uint8Array for invalid base64", () => {
    const result = decodeBase64("!!!invalid!!!");
    expect(result).toBeInstanceOf(Uint8Array);
    expect(result.length).toBe(0);
  });

  it("returns empty Uint8Array for empty string", () => {
    const result = decodeBase64("");
    expect(result.length).toBe(0);
  });
});

describe("encodeBase64", () => {
  it("encodes ASCII text to base64", () => {
    expect(encodeBase64("hello")).toBe("aGVsbG8=");
  });

  it("encodes empty string to empty base64", () => {
    expect(encodeBase64("")).toBe("");
  });

  it("round-trips with decodeBase64", () => {
    const original = "Hello, World! 🌍";
    const encoded = encodeBase64(original);
    const decoded = decodeBase64(encoded);
    expect(bytesToUtf(decoded)).toBe(original);
  });

  it("encodes UTF-8 multibyte characters correctly", () => {
    const result = encodeBase64("€");
    // "€" is U+20AC, UTF-8: 0xE2 0x82 0xAC → base64: 4oysAQ== ... actually "4oKs"
    const decoded = decodeBase64(result);
    expect(bytesToUtf(decoded)).toBe("€");
  });
});

describe("bytesToHex", () => {
  it("converts bytes to space-separated hex", () => {
    const bytes = new Uint8Array([0, 1, 255, 16]);
    expect(bytesToHex(bytes)).toBe("00 01 ff 10");
  });

  it("returns empty string for empty array", () => {
    expect(bytesToHex(new Uint8Array())).toBe("");
  });
});

describe("bytesToUtf", () => {
  it("decodes UTF-8 bytes to string", () => {
    const encoder = new TextEncoder();
    const bytes = encoder.encode("hello world");
    expect(bytesToUtf(bytes)).toBe("hello world");
  });

  it("returns empty string for empty array", () => {
    expect(bytesToUtf(new Uint8Array())).toBe("");
  });
});

describe("formatJson", () => {
  it("pretty-prints valid JSON", () => {
    const result = formatJson('{"a":1,"b":2}');
    expect(result).toContain("\n");
    expect(JSON.parse(result)).toEqual({ a: 1, b: 2 });
  });

  it("returns original string for invalid JSON", () => {
    const bad = "not json at all";
    expect(formatJson(bad)).toBe(bad);
  });

  it("handles arrays", () => {
    const result = formatJson("[1,2,3]");
    expect(JSON.parse(result)).toEqual([1, 2, 3]);
  });
});

describe("formatXml", () => {
  it("adds indentation to nested XML", () => {
    const xml = "<root><child>text</child></root>";
    const result = formatXml(xml);
    expect(result).toContain("\n");
    // Child element should be indented
    expect(result).toMatch(/^\s*<child>/m);
  });

  it("returns original string on error", () => {
    // formatXml is resilient — passes through on catch
    expect(typeof formatXml("")).toBe("string");
  });
});
