//! Decoding of captured HTTP/1.1 bodies for display: chunked transfer coding
//! and `Content-Encoding` (gzip, deflate, br).
//!
//! Bodies can be decoded in one go or incrementally as they stream in, and
//! decoding is tolerant of truncated input, because captured bodies are cut
//! off at the configured size limit.

use std::collections::HashMap;
use std::io::{self, Write};

use brotli_decompressor::DecompressorWriter;
use flate2::write::{DeflateDecoder, MultiGzDecoder, ZlibDecoder};

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

/// Incremental parser for `Transfer-Encoding: chunked` framing, fed with the
/// body bytes as they arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkedFraming {
    /// Reading a chunk-size line.  `in_ext` is set once a non-hex byte (the
    /// start of a chunk extension) was seen; the rest of the line is ignored.
    Size {
        size: usize,
        in_ext: bool,
    },
    Data {
        remaining: usize,
    },
    /// Skipping the line break after chunk data.
    DataEnd,
    /// Reading the trailer section after the last chunk, which ends with an
    /// empty line.
    Trailer {
        line_len: usize,
    },
}

impl ChunkedFraming {
    pub const START: Self = Self::Size {
        size: 0,
        in_ext: false,
    };

    /// Consumes `data`.  Returns `Some(n)` when the message ends after the
    /// first `n` bytes, or `None` when all of `data` belongs to the message.
    pub fn advance(&mut self, data: &[u8]) -> Option<usize> {
        self.advance_with(data, |_| {})
    }

    /// Like [`advance`](Self::advance), passing the chunk data to `on_data`.
    pub fn advance_with(&mut self, data: &[u8], mut on_data: impl FnMut(&[u8])) -> Option<usize> {
        let mut i = 0;
        while i < data.len() {
            match *self {
                Self::Data { remaining } => {
                    let take = remaining.min(data.len() - i);
                    on_data(&data[i..i + take]);
                    i += take;
                    *self = if take == remaining {
                        Self::DataEnd
                    } else {
                        Self::Data {
                            remaining: remaining - take,
                        }
                    };
                }
                Self::Size { size, in_ext } => {
                    let byte = data[i];
                    i += 1;
                    *self = match byte {
                        b'\n' if size == 0 => Self::Trailer { line_len: 0 },
                        b'\n' => Self::Data { remaining: size },
                        b'\r' => continue,
                        _ if in_ext => continue,
                        _ => match (byte as char).to_digit(16) {
                            Some(digit) => Self::Size {
                                size: size.saturating_mul(16).saturating_add(digit as usize),
                                in_ext,
                            },
                            None => Self::Size { size, in_ext: true },
                        },
                    };
                }
                Self::DataEnd => {
                    if data[i] == b'\n' {
                        *self = Self::START;
                    }
                    i += 1;
                }
                Self::Trailer { line_len } => {
                    let byte = data[i];
                    i += 1;
                    match byte {
                        b'\n' if line_len == 0 => {
                            *self = Self::START;
                            return Some(i);
                        }
                        b'\n' => *self = Self::Trailer { line_len: 0 },
                        b'\r' => {}
                        _ => {
                            *self = Self::Trailer {
                                line_len: line_len + 1,
                            }
                        }
                    }
                }
            }
        }
        None
    }
}

/// Removes chunked transfer coding framing, returning the concatenated chunk
/// data. A truncated final chunk contributes the bytes that are present.
pub fn dechunk(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut framing = ChunkedFraming::START;
    framing.advance_with(data, |chunk| out.extend_from_slice(chunk));
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
    let stages = content_stages(headers);
    if stages.is_empty() || body.is_empty() {
        return body;
    }
    let mut decoder = BodyDecoder {
        chunked: None,
        stages,
        remaining: limit,
    };
    let mut decoded = decoder.push(&body);
    decoded.extend(decoder.finish());
    decoded
}

/// [`decode_response_body`] on the blocking thread pool, since decompressing a
/// large body is CPU-bound and must not stall the proxy's async workers.
pub async fn decode_response_body_async(
    headers: &HashMap<String, String>,
    body: Vec<u8>,
    limit: usize,
) -> Vec<u8> {
    if content_stages(headers).is_empty() {
        return body;
    }
    let headers = headers.clone();
    tokio::task::spawn_blocking(move || decode_response_body(&headers, body, limit))
        .await
        .unwrap_or_default()
}

/// Decodes a response body incrementally as its raw bytes arrive: removes
/// chunked framing and undoes the `Content-Encoding`, producing at most
/// `limit` bytes in total.
pub struct BodyDecoder {
    chunked: Option<ChunkedFraming>,
    /// Decoders for the content codings, in the order they must be undone.
    stages: Vec<ContentStage>,
    /// Output bytes left before reaching the limit.
    remaining: usize,
}

impl BodyDecoder {
    pub fn new(headers: &HashMap<String, String>, limit: usize) -> Self {
        Self {
            chunked: header_value(headers, "transfer-encoding")
                .is_some_and(is_chunked)
                .then_some(ChunkedFraming::START),
            stages: content_stages(headers),
            remaining: limit,
        }
    }

    /// Decodes the next raw body bytes, returning the newly decoded bytes.
    pub fn push(&mut self, raw: &[u8]) -> Vec<u8> {
        if self.remaining == 0 {
            return Vec::new();
        }
        let mut data = match &mut self.chunked {
            Some(framing) => {
                let mut out = Vec::with_capacity(raw.len());
                framing.advance_with(raw, |chunk| out.extend_from_slice(chunk));
                out
            }
            None => raw.to_vec(),
        };
        for stage in &mut self.stages {
            if data.is_empty() {
                break;
            }
            data = stage.push(&data);
        }
        self.within_limit(data)
    }

    /// Returns the output still held by the decoders at the end of the body.
    pub fn finish(&mut self) -> Vec<u8> {
        let mut data = Vec::new();
        for stage in &mut self.stages {
            let mut out = if data.is_empty() {
                Vec::new()
            } else {
                stage.push(&data)
            };
            out.extend(stage.finish());
            data = out;
        }
        self.within_limit(data)
    }

    fn within_limit(&mut self, mut data: Vec<u8>) -> Vec<u8> {
        data.truncate(self.remaining);
        self.remaining -= data.len();
        data
    }
}

/// Decoders for the comma-separated `Content-Encoding` codings, which were
/// applied in order and are therefore undone in reverse.  Empty when there is
/// nothing to decode, or when a coding is unsupported (the body is then kept
/// as-is).
fn content_stages(headers: &HashMap<String, String>) -> Vec<ContentStage> {
    let Some(encoding) = header_value(headers, "content-encoding") else {
        return Vec::new();
    };
    let mut stages = Vec::new();
    for coding in encoding.rsplit(',').map(str::trim) {
        if coding.is_empty() || coding.eq_ignore_ascii_case("identity") {
            continue;
        }
        let Some(decoder) = ContentDecoder::for_coding(coding) else {
            return Vec::new();
        };
        stages.push(ContentStage {
            decoder,
            undecoded: Some(Vec::new()),
        });
    }
    stages
}

/// One content coding being undone.
struct ContentStage {
    decoder: ContentDecoder,
    /// Input kept until the decoder produces output, so that data which turns
    /// out not to be encoded after all can be passed through as-is.
    undecoded: Option<Vec<u8>>,
}

impl ContentStage {
    fn push(&mut self, input: &[u8]) -> Vec<u8> {
        if let Some(undecoded) = &mut self.undecoded {
            undecoded.extend_from_slice(input);
        }
        let result = self.decoder.write(input);
        self.settle(result)
    }

    fn finish(&mut self) -> Vec<u8> {
        let result = self.decoder.finish();
        self.settle(result)
    }

    fn settle(&mut self, result: io::Result<Vec<u8>>) -> Vec<u8> {
        match result {
            Ok(out) => {
                if !out.is_empty() {
                    self.undecoded = None;
                }
                out
            }
            Err(_) => match self.undecoded.take() {
                Some(raw) => {
                    self.decoder = ContentDecoder::Passthrough;
                    raw
                }
                None => {
                    self.decoder = ContentDecoder::Failed;
                    Vec::new()
                }
            },
        }
    }
}

enum ContentDecoder {
    Gzip(MultiGzDecoder<Vec<u8>>),
    Zlib(ZlibDecoder<Vec<u8>>),
    RawDeflate(DeflateDecoder<Vec<u8>>),
    /// `deflate` before its first two bytes, which tell zlib-wrapped data
    /// (as the spec requires) from raw deflate (as some servers send), arrived.
    Deflate(Vec<u8>),
    Brotli(Box<DecompressorWriter<Vec<u8>>>),
    /// The data turned out not to be encoded and is passed through.
    Passthrough,
    /// Decoding failed after producing output; the rest is dropped.
    Failed,
}

impl ContentDecoder {
    fn for_coding(coding: &str) -> Option<Self> {
        Some(match coding.to_ascii_lowercase().as_str() {
            "gzip" | "x-gzip" => Self::Gzip(MultiGzDecoder::new(Vec::new())),
            "deflate" => Self::Deflate(Vec::new()),
            "br" => Self::Brotli(Box::new(DecompressorWriter::new(Vec::new(), 4096))),
            _ => return None,
        })
    }

    /// Feeds `input` and returns the output it produced.
    fn write(&mut self, input: &[u8]) -> io::Result<Vec<u8>> {
        match self {
            Self::Gzip(d) => write_and_take(d, input, MultiGzDecoder::get_mut),
            Self::Zlib(d) => write_and_take(d, input, ZlibDecoder::get_mut),
            Self::RawDeflate(d) => write_and_take(d, input, DeflateDecoder::get_mut),
            Self::Brotli(d) => write_and_take(&mut **d, input, DecompressorWriter::get_mut),
            Self::Deflate(head) => {
                head.extend_from_slice(input);
                if head.len() < 2 {
                    return Ok(Vec::new());
                }
                let head = std::mem::take(head);
                // RFC 1950: compression method 8, and the 16-bit header is a
                // multiple of 31.
                let zlib = head[0] & 0x0f == 8 && u16::from_be_bytes([head[0], head[1]]) % 31 == 0;
                *self = if zlib {
                    Self::Zlib(ZlibDecoder::new(Vec::new()))
                } else {
                    Self::RawDeflate(DeflateDecoder::new(Vec::new()))
                };
                self.write(&head)
            }
            Self::Passthrough => Ok(input.to_vec()),
            Self::Failed => Ok(Vec::new()),
        }
    }

    /// Ends the stream and returns the remaining output.
    fn finish(&mut self) -> io::Result<Vec<u8>> {
        match self {
            Self::Gzip(d) => d.try_finish().map(|()| std::mem::take(d.get_mut())),
            Self::Zlib(d) => d.try_finish().map(|()| std::mem::take(d.get_mut())),
            Self::RawDeflate(d) => d.try_finish().map(|()| std::mem::take(d.get_mut())),
            Self::Brotli(d) => d.close().map(|()| std::mem::take(d.get_mut())),
            Self::Deflate(_) => Err(io::ErrorKind::UnexpectedEof.into()),
            Self::Passthrough | Self::Failed => Ok(Vec::new()),
        }
    }
}

/// Writes `input` to a decoder, flushes it, and takes the output collected in
/// its inner buffer.
fn write_and_take<D: Write>(
    decoder: &mut D,
    input: &[u8],
    inner: fn(&mut D) -> &mut Vec<u8>,
) -> io::Result<Vec<u8>> {
    decoder.write_all(input)?;
    decoder.flush()?;
    Ok(std::mem::take(inner(decoder)))
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

    // ── content_stages ────────────────────────────────────────────────────

    #[test]
    fn content_stages_skip_identity_and_unknown_codings() {
        assert_eq!(
            content_stages(&headers(&[("Content-Encoding", "br")])).len(),
            1
        );
        assert_eq!(
            content_stages(&headers(&[("Content-Encoding", "gzip, br")])).len(),
            2
        );
        assert!(content_stages(&headers(&[("Content-Encoding", "identity")])).is_empty());
        assert!(content_stages(&headers(&[("Content-Encoding", "gzip, zstd")])).is_empty());
        assert!(content_stages(&headers(&[])).is_empty());
    }

    // ── ChunkedFraming ────────────────────────────────────────────────────

    #[test]
    fn chunked_framing_finds_message_end() {
        let body = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\nNEXT";
        let mut framing = ChunkedFraming::START;
        assert_eq!(framing.advance(body), Some(body.len() - 4));
        assert_eq!(framing, ChunkedFraming::START);
    }

    #[test]
    fn chunked_framing_byte_by_byte() {
        let body = b"a;ext=1\r\n0123456789\r\n0\r\nTrailer: x\r\n\r\n";
        let mut framing = ChunkedFraming::START;
        let mut data = Vec::new();
        for (i, byte) in body.iter().enumerate() {
            let result =
                framing.advance_with(std::slice::from_ref(byte), |d| data.extend_from_slice(d));
            if i == body.len() - 1 {
                assert_eq!(result, Some(1));
            } else {
                assert_eq!(result, None, "byte {i}");
            }
        }
        assert_eq!(data, b"0123456789");
    }

    #[test]
    fn chunked_framing_chunk_data_resembling_terminator() {
        // Chunk data containing "0\r\n\r\n" must not end the message.
        let body = b"5\r\n0\r\n\r\n\r\n0\r\n\r\n";
        let mut framing = ChunkedFraming::START;
        assert_eq!(framing.advance(body), Some(body.len()));
    }

    #[test]
    fn chunked_framing_incomplete() {
        let mut framing = ChunkedFraming::START;
        assert_eq!(framing.advance(b"10\r\nonly part"), None);
        assert_eq!(framing, ChunkedFraming::Data { remaining: 7 });
    }

    // ── BodyDecoder (incremental) ─────────────────────────────────────────

    fn chunked(data: &[u8], chunk_size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in data.chunks(chunk_size) {
            out.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            out.extend_from_slice(chunk);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
        out
    }

    /// Feeds `raw` to a decoder in pieces of `piece` bytes and returns the
    /// output of each push, plus the output of `finish`.
    fn decode_in_pieces(h: &HashMap<String, String>, raw: &[u8], piece: usize) -> Vec<Vec<u8>> {
        let mut decoder = BodyDecoder::new(h, usize::MAX);
        let mut outputs: Vec<Vec<u8>> = raw.chunks(piece).map(|p| decoder.push(p)).collect();
        outputs.push(decoder.finish());
        outputs
    }

    #[test]
    fn body_decoder_dechunks_incrementally() {
        let h = headers(&[("Transfer-Encoding", "chunked")]);
        let raw = chunked(b"data: one\n\ndata: two\n\n", 11);
        for piece in [1, 5, 64] {
            let outputs = decode_in_pieces(&h, &raw, piece);
            assert_eq!(
                outputs.concat(),
                b"data: one\n\ndata: two\n\n",
                "piece {piece}"
            );
        }
    }

    #[test]
    fn body_decoder_emits_events_as_they_arrive() {
        let h = headers(&[("Transfer-Encoding", "chunked")]);
        let mut decoder = BodyDecoder::new(&h, usize::MAX);
        assert_eq!(decoder.push(b"b\r\ndata: one\n\n\r\n"), b"data: one\n\n");
        assert_eq!(decoder.push(b"b\r\ndata: two\n\n\r\n"), b"data: two\n\n");
        assert_eq!(decoder.push(b"0\r\n\r\n"), b"");
        assert_eq!(decoder.finish(), b"");
    }

    #[test]
    fn body_decoder_passes_identity_bodies_through() {
        let mut decoder = BodyDecoder::new(&headers(&[]), usize::MAX);
        assert_eq!(decoder.push(b"until close"), b"until close");
        assert_eq!(decoder.finish(), b"");
    }

    /// Compresses `parts` as one stream, flushing after each, so every part
    /// can be decoded as soon as it arrives (as streaming servers do).
    fn gzip_flushed(parts: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut pieces = Vec::new();
        for part in parts {
            enc.write_all(part).unwrap();
            enc.flush().unwrap();
            pieces.push(std::mem::take(enc.get_mut()));
        }
        pieces.push(enc.finish().unwrap());
        pieces
    }

    #[test]
    fn body_decoder_decompresses_gzip_streams_incrementally() {
        let h = headers(&[("Content-Encoding", "gzip")]);
        let pieces = gzip_flushed(&[b"data: one\n\n", b"data: two\n\n"]);
        let mut decoder = BodyDecoder::new(&h, usize::MAX);
        assert_eq!(decoder.push(&pieces[0]), b"data: one\n\n");
        assert_eq!(decoder.push(&pieces[1]), b"data: two\n\n");
        let mut tail = decoder.push(&pieces[2]);
        tail.extend(decoder.finish());
        assert_eq!(tail, b"");
    }

    #[test]
    fn body_decoder_decompresses_chunked_brotli_byte_by_byte() {
        let h = headers(&[("Content-Encoding", "br"), ("Transfer-Encoding", "chunked")]);
        let text = b"{\"streamed\":\"brotli\"}".repeat(20);
        let outputs = decode_in_pieces(&h, &chunked(&brotli(&text), 7), 1);
        assert_eq!(outputs.concat(), text);
    }

    #[test]
    fn body_decoder_detects_zlib_and_raw_deflate_across_pushes() {
        let h = headers(&[("Content-Encoding", "deflate")]);
        assert_eq!(decode_in_pieces(&h, &zlib(b"zlib"), 1).concat(), b"zlib");
        assert_eq!(
            decode_in_pieces(&h, &raw_deflate(b"raw"), 1).concat(),
            b"raw"
        );
    }

    #[test]
    fn body_decoder_passes_undecodable_data_through() {
        let h = headers(&[("Content-Encoding", "gzip")]);
        assert_eq!(
            decode_in_pieces(&h, b"plain text", 3).concat(),
            b"plain text"
        );
    }

    #[test]
    fn body_decoder_respects_the_limit() {
        let h = headers(&[("Transfer-Encoding", "chunked")]);
        let mut decoder = BodyDecoder::new(&h, 5);
        assert_eq!(decoder.push(b"3\r\nabc\r\n"), b"abc");
        assert_eq!(decoder.push(b"3\r\ndef\r\n"), b"de");
        assert_eq!(decoder.push(b"3\r\nghi\r\n"), b"");
    }
}
