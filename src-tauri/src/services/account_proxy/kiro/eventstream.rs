use super::ProxyError;
use bytes::{Bytes, BytesMut};
use std::collections::{BTreeMap, BTreeSet};

const BUFFER_LIMIT: usize = 32 * 1024 * 1024;

pub(super) struct Frame {
    pub(super) headers: BTreeMap<String, String>,
    pub(super) payload: Bytes,
}

#[derive(Default)]
pub(super) struct Parser {
    buffer: BytesMut,
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

fn word(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().expect("checked four-byte field"))
}

impl Parser {
    pub(super) fn push(&mut self, bytes: &[u8]) -> Result<(), ProxyError> {
        if self.buffer.len().saturating_add(bytes.len()) > BUFFER_LIMIT {
            return Err(ProxyError::upstream(
                "Kiro event stream exceeded its buffer limit",
            ));
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    pub(super) fn next(&mut self) -> Result<Option<Frame>, ProxyError> {
        if self.buffer.len() < 12 {
            return Ok(None);
        }
        if crc32(&self.buffer[..8]) != word(&self.buffer[8..12]) {
            return Err(ProxyError::upstream("Invalid Kiro event prelude checksum"));
        }
        let total = word(&self.buffer[..4]) as usize;
        let headers = word(&self.buffer[4..8]) as usize;
        if !(16..=BUFFER_LIMIT).contains(&total) || headers > total - 16 {
            return Err(ProxyError::upstream("Invalid Kiro event lengths"));
        }
        if self.buffer.len() < total {
            return Ok(None);
        }
        if crc32(&self.buffer[..total - 4]) != word(&self.buffer[total - 4..total]) {
            return Err(ProxyError::upstream("Invalid Kiro event checksum"));
        }
        let bytes = self.buffer.split_to(total).freeze();
        Ok(Some(Frame {
            headers: parse_headers(&bytes[12..12 + headers])?,
            payload: bytes.slice(12 + headers..total - 4),
        }))
    }

    pub(super) fn finish(&self) -> Result<(), ProxyError> {
        if self.buffer.is_empty() {
            Ok(())
        } else {
            Err(ProxyError::upstream(
                "Kiro event stream ended inside a frame",
            ))
        }
    }
}

fn parse_headers(mut bytes: &[u8]) -> Result<BTreeMap<String, String>, ProxyError> {
    let invalid = || ProxyError::upstream("Invalid Kiro event headers");
    let mut names = BTreeSet::new();
    let mut headers = BTreeMap::new();
    while !bytes.is_empty() {
        let length = usize::from(bytes[0]);
        if length == 0 || bytes.len() < length + 2 {
            return Err(invalid());
        }
        let name = std::str::from_utf8(&bytes[1..length + 1])
            .map_err(|_| invalid())?
            .to_owned();
        if !names.insert(name.clone()) {
            return Err(invalid());
        }
        let kind = bytes[length + 1];
        bytes = &bytes[length + 2..];
        let size = match kind {
            0 | 1 => 0,
            2 => 1,
            3 => 2,
            4 => 4,
            5 | 8 => 8,
            9 => 16,
            6 | 7 => {
                if bytes.len() < 2 {
                    return Err(invalid());
                }
                let size = usize::from(u16::from_be_bytes([bytes[0], bytes[1]]));
                bytes = &bytes[2..];
                size
            }
            _ => return Err(invalid()),
        };
        if bytes.len() < size {
            return Err(invalid());
        }
        if kind == 7 {
            headers.insert(
                name,
                std::str::from_utf8(&bytes[..size])
                    .map_err(|_| invalid())?
                    .into(),
            );
        }
        bytes = &bytes[size..];
    }
    Ok(headers)
}

#[cfg(test)]
pub(super) fn frame(message: &str, kind: &str, payload: &serde_json::Value) -> Vec<u8> {
    raw_frame(
        &[
            (":message-type", message),
            (
                if message == "exception" {
                    ":exception-type"
                } else {
                    ":event-type"
                },
                kind,
            ),
        ],
        payload.to_string().as_bytes(),
    )
}

#[cfg(test)]
pub(super) fn raw_frame(values: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut headers = Vec::new();
    for (name, value) in values {
        headers.push(name.len() as u8);
        headers.extend_from_slice(name.as_bytes());
        headers.push(7);
        headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
        headers.extend_from_slice(value.as_bytes());
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&((16 + headers.len() + payload.len()) as u32).to_be_bytes());
    bytes.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&crc32(&bytes).to_be_bytes());
    bytes.extend_from_slice(&headers);
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(&crc32(&bytes).to_be_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn known_crc_and_every_chunk_boundary_decode_real_utf8_payloads() {
        assert_eq!(crc32(b"123456789"), 0xcbf43926);
        let encoded = frame(
            "event",
            "assistantResponseEvent",
            &json!({"content":"中文"}),
        );
        for boundary in 0..encoded.len() {
            let mut parser = Parser::default();
            parser.push(&encoded[..boundary]).unwrap();
            assert!(parser.next().unwrap().is_none());
            parser.push(&encoded[boundary..]).unwrap();
            let decoded = parser.next().unwrap().unwrap();
            assert_eq!(decoded.headers[":event-type"], "assistantResponseEvent");
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&decoded.payload).unwrap()["content"],
                "中文"
            );
            assert!(parser.next().unwrap().is_none());
            parser.finish().unwrap();
        }
    }

    #[test]
    fn corruption_truncation_and_duplicate_or_typed_invalid_headers_fail() {
        let encoded = frame(
            "event",
            "assistantResponseEvent",
            &json!({"content":"valid"}),
        );
        for byte in [0, 8, 20, encoded.len() - 1] {
            let mut corrupt = encoded.clone();
            corrupt[byte] ^= 1;
            let mut parser = Parser::default();
            parser.push(&corrupt).unwrap();
            assert!(parser.next().is_err());
        }
        let mut parser = Parser::default();
        parser.push(&encoded[..encoded.len() - 1]).unwrap();
        assert!(parser.next().unwrap().is_none());
        assert!(parser.finish().is_err());
        assert!(parse_headers(&[1, b'a', 7, 0, 2, b'x']).is_err());
        assert!(parse_headers(&[1, b'a', 0, 1, b'a', 1]).is_err());
        assert!(parse_headers(&[1, b'a', 10]).is_err());
        assert!(parse_headers(&[1, b'a', 7, 0, 1, 0xff]).is_err());
        assert!(parse_headers(&[1, b'a', 9, 0, 0]).is_err());
        assert!(
            parse_headers(&[1, b'a', 0, 1, b'b', 1, 1, b'c', 2, 5, 1, b'd', 7, 0, 1, b'x'])
                .unwrap()
                .contains_key("d")
        );
    }
}
