//! Decoding of captured HTTP/1.1 bodies for display: chunked transfer coding
//! and `Content-Encoding` (gzip, deflate, br).
//!
//! All functions are tolerant of truncated input, because captured bodies are
//! cut off at the configured size limit.

use std::collections::HashMap;
use std::io::Read;

/// Case-insensitive header lookup.
pub fn header_value<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Returns `true` when a `Transfer-Encoding` value ends with `chunked`, which
/// is what determines the message framing (RFC 9112 §6.3).
pub fn is_chunked(transfer_encoding: &str) -> bool {
    transfer_encoding
        .rsplit(',')
        .next()
        .is_some_and(|last| last.trim().eq_ignore_ascii_case("chunked"))
}

/// Removes chunked transfer coding framing, returning the concatenated chunk
/// data. A truncated final chunk contributes the bytes that are present.
pub fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    while let Some(line_end) = data.iter().position(|&b| b == b'\n') {
        let size_line = String::from_utf8_lossy(&data[..line_end]);
        let size_str = size_line.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_str, 16) else {
            break;
        };
        if size == 0 {
            break;
        }
        data = &data[line_end + 1..];
        let take = size.min(data.len());
        out.extend_from_slice(&data[..take]);
        data = &data[take..];
        if take < size {
            break;
        }
        data = data
            .strip_prefix(b"\r\n")
            .or_else(|| data.strip_prefix(b"\n"))
            .unwrap_or(data);
    }
    out
}

/// Undoes the `Content-Encoding` of a response body for display, producing at
/// most `limit` bytes.
///
/// Returns the body unchanged when it has no (or an unsupported) encoding or
/// cannot be decoded at all. A truncated body yields the part that decodes.
pub fn decode_response_body(
    headers: &HashMap<String, String>,
    body: Vec<u8>,
    limit: usize,
) -> Vec<u8> {
    match header_value(headers, "content-encoding") {
        Some(encoding) if !body.is_empty() => {
            decode_content_encoding(encoding, &body, limit).unwrap_or(body)
        }
        _ => body,
    }
}

/// [`decode_response_body`] on the blocking thread pool, since decompressing a
/// large body is CPU-bound and must not stall the proxy's async workers.
pub async fn decode_response_body_async(
    headers: &HashMap<String, String>,
    body: Vec<u8>,
    limit: usize,
) -> Vec<u8> {
    if !has_content_encoding(headers) {
        return body;
    }
    let headers = headers.clone();
    tokio::task::spawn_blocking(move || decode_response_body(&headers, body, limit))
        .await
        .unwrap_or_default()
}

/// Returns `true` when `headers` declare a `Content-Encoding` other than identity.
fn has_content_encoding(headers: &HashMap<String, String>) -> bool {
    header_value(headers, "content-encoding").is_some_and(|encoding| {
        encoding
            .split(',')
            .any(|c| !c.trim().is_empty() && !c.trim().eq_ignore_ascii_case("identity"))
    })
}

/// Decodes a body with the comma-separated codings in `encoding`, which were
/// applied in order, so they are undone in reverse.
fn decode_content_encoding(encoding: &str, body: &[u8], limit: usize) -> Option<Vec<u8>> {
    let mut data = body.to_vec();
    for coding in encoding.rsplit(',').map(str::trim) {
        data = match coding.to_ascii_lowercase().as_str() {
            "" | "identity" => continue,
            "gzip" | "x-gzip" => read_lossy(flate2::read::MultiGzDecoder::new(&data[..]), limit)?,
            // `deflate` is zlib-wrapped by spec, but some servers send raw deflate.
            "deflate" => read_lossy(flate2::read::ZlibDecoder::new(&data[..]), limit)
                .or_else(|| read_lossy(flate2::read::DeflateDecoder::new(&data[..]), limit))?,
            "br" => read_lossy(
                brotli_decompressor::Decompressor::new(&data[..], 4096),
                limit,
            )?,
            _ => return None,
        };
    }
    Some(data)
}

/// Reads up to `limit` bytes, keeping what was decoded before an error (such as
/// the unexpected end of a truncated stream). Returns `None` when nothing could
/// be decoded.
fn read_lossy(reader: impl Read, limit: usize) -> Option<Vec<u8>> {
    let mut reader = reader.take(limit as u64);
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Some(out),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return (!out.is_empty()).then_some(out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    fn raw_deflate(data: &[u8]) -> Vec<u8> {
        let mut enc =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    fn brotli(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        brotli::BrotliCompress(&mut &data[..], &mut out, &Default::default()).unwrap();
        out
    }

    // ── header_value / is_chunked ─────────────────────────────────────────

    #[test]
    fn header_value_is_case_insensitive() {
        let h = headers(&[("Content-Encoding", "gzip")]);
        assert_eq!(header_value(&h, "content-encoding"), Some("gzip"));
        assert_eq!(header_value(&h, "CONTENT-ENCODING"), Some("gzip"));
        assert_eq!(header_value(&h, "content-type"), None);
    }

    #[test]
    fn is_chunked_checks_the_final_coding() {
        assert!(is_chunked("chunked"));
        assert!(is_chunked("Chunked"));
        assert!(is_chunked("gzip, chunked"));
        assert!(!is_chunked("chunked, gzip"));
        assert!(!is_chunked("gzip"));
        assert!(!is_chunked(""));
    }

    // ── dechunk ───────────────────────────────────────────────────────────

    #[test]
    fn dechunk_concatenates_chunks() {
        assert_eq!(
            dechunk(b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"),
            b"hello world"
        );
    }

    #[test]
    fn dechunk_ignores_extensions_and_trailers() {
        let data = b"5;name=value\r\nhello\r\n0\r\nExpires: never\r\n\r\n";
        assert_eq!(dechunk(data), b"hello");
    }

    #[test]
    fn dechunk_accepts_uppercase_hex_and_bare_lf() {
        let mut data = b"A\n".to_vec();
        data.extend_from_slice(b"0123456789\n0\n\n");
        assert_eq!(dechunk(&data), b"0123456789");
    }

    #[test]
    fn dechunk_keeps_a_truncated_final_chunk() {
        assert_eq!(dechunk(b"5\r\nhello\r\n10\r\nabc"), b"helloabc");
    }

    #[test]
    fn dechunk_stops_at_an_incomplete_size_line() {
        assert_eq!(dechunk(b"5\r\nhello\r\n1"), b"hello");
    }

    #[test]
    fn dechunk_stops_at_invalid_size() {
        assert_eq!(dechunk(b"zz\r\nhello\r\n"), b"");
    }

    #[test]
    fn dechunk_preserves_binary_data() {
        let payload = [0x00, 0x0d, 0x0a, 0xff, 0x0a];
        let mut data = b"5\r\n".to_vec();
        data.extend_from_slice(&payload);
        data.extend_from_slice(b"\r\n0\r\n\r\n");
        assert_eq!(dechunk(&data), payload);
    }

    // ── decode_response_body ──────────────────────────────────────────────

    #[test]
    fn decode_without_content_encoding_is_identity() {
        let h = headers(&[("Content-Type", "application/json")]);
        assert_eq!(decode_response_body(&h, b"{}".to_vec(), 1024), b"{}");
    }

    #[test]
    fn decode_gzip() {
        let h = headers(&[("Content-Encoding", "gzip")]);
        assert_eq!(
            decode_response_body(&h, gzip(b"{\"ok\":true}"), 1024),
            b"{\"ok\":true}"
        );
    }

    #[test]
    fn decode_x_gzip() {
        let h = headers(&[("content-encoding", "x-gzip")]);
        assert_eq!(decode_response_body(&h, gzip(b"hi"), 1024), b"hi");
    }

    #[test]
    fn decode_zlib_deflate() {
        let h = headers(&[("Content-Encoding", "deflate")]);
        assert_eq!(decode_response_body(&h, zlib(b"hello"), 1024), b"hello");
    }

    #[test]
    fn decode_raw_deflate() {
        let h = headers(&[("Content-Encoding", "deflate")]);
        assert_eq!(
            decode_response_body(&h, raw_deflate(b"hello"), 1024),
            b"hello"
        );
    }

    #[test]
    fn decode_brotli() {
        let h = headers(&[("Content-Encoding", "br")]);
        assert_eq!(
            decode_response_body(&h, brotli(b"{\"ok\":true}"), 1024),
            b"{\"ok\":true}"
        );
    }

    #[test]
    fn decode_multiple_codings_in_reverse_order() {
        let h = headers(&[("Content-Encoding", "deflate, gzip")]);
        let body = gzip(&zlib(b"layered"));
        assert_eq!(decode_response_body(&h, body, 1024), b"layered");
    }

    #[test]
    fn decode_identity_is_a_no_op() {
        let h = headers(&[("Content-Encoding", "identity")]);
        assert_eq!(decode_response_body(&h, b"plain".to_vec(), 1024), b"plain");
    }

    #[test]
    fn decode_unknown_encoding_keeps_the_body() {
        let h = headers(&[("Content-Encoding", "zstd")]);
        assert_eq!(
            decode_response_body(&h, b"\x28\xb5".to_vec(), 1024),
            b"\x28\xb5"
        );
    }

    #[test]
    fn decode_invalid_data_keeps_the_body() {
        let h = headers(&[("Content-Encoding", "gzip")]);
        assert_eq!(
            decode_response_body(&h, b"not gzip".to_vec(), 1024),
            b"not gzip"
        );
    }

    #[test]
    fn decode_empty_body() {
        let h = headers(&[("Content-Encoding", "gzip")]);
        assert!(decode_response_body(&h, Vec::new(), 1024).is_empty());
    }

    #[test]
    fn decode_truncated_gzip_keeps_the_decoded_prefix() {
        let original: Vec<u8> = (0..20_000u32).flat_map(|i| i.to_le_bytes()).collect();
        let compressed = gzip(&original);
        let truncated = compressed[..compressed.len() / 2].to_vec();
        let h = headers(&[("Content-Encoding", "gzip")]);
        let decoded = decode_response_body(&h, truncated, usize::MAX);
        assert!(!decoded.is_empty());
        assert!(decoded.len() < original.len());
        assert_eq!(decoded, original[..decoded.len()]);
    }

    #[test]
    fn decode_output_is_capped_at_limit() {
        let original = vec![b'a'; 100_000];
        let h = headers(&[("Content-Encoding", "gzip")]);
        let decoded = decode_response_body(&h, gzip(&original), 1000);
        assert_eq!(decoded, vec![b'a'; 1000]);
    }

    // ── has_content_encoding ──────────────────────────────────────────────

    #[test]
    fn has_content_encoding_detects_real_codings() {
        assert!(has_content_encoding(&headers(&[(
            "Content-Encoding",
            "br"
        )])));
        assert!(!has_content_encoding(&headers(&[(
            "Content-Encoding",
            "identity"
        )])));
        assert!(!has_content_encoding(&headers(&[])));
    }
}
