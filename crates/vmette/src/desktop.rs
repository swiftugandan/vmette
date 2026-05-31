//! Desktop computer-use protocol: the **framed request/response codec** that
//! carries the [`crate::Action`] vocabulary between the host [`crate::Session`]
//! (Agent workload) and the in-guest `vmette-desktop-agent`.
//!
//! The *types* on the wire ([`Action`], [`ResponseHeader`], [`ScrollDirection`])
//! are the host↔guest contract and live in `vmette-proto`; this module owns
//! only the framing — a blocking round-trip over any `Read`/`Write`, pure (no
//! VZ, no objc2) and unit-testable in isolation. The headless one-shot path
//! never touches it.
//!
//! ## Wire format
//!
//! ```text
//! [u32 LE header_len][header JSON bytes][payload bytes]
//! ```
//!
//! A single little-endian `u32` prefixes the JSON header. Screenshots and
//! any other binary results travel as a raw payload *after* the header; the
//! header's `payload_len` says how many payload bytes follow (0 for none).
//! Requests carry no payload. Keeping one length prefix (not two) matches
//! the guest C agent's simpler `read(u32) → read(header) → read(payload)`.

use std::io::{self, Read, Write};

// The action vocabulary + response header are the host↔guest wire *contract*,
// owned by `vmette-proto`. This module owns only the framing codec that moves
// them over the vsock; re-export the types so the library's public API (and
// `crate::Action` / `crate::ResponseHeader`) stay one import away.
pub use vmette_proto::{Action, ResponseHeader, ScrollDirection};

/// Maximum header length we will accept off the wire (1 MiB). Guards a
/// corrupt/hostile length prefix from triggering a huge allocation. The
/// JSON header is tiny in practice; payloads are bounded separately.
const MAX_HEADER_LEN: u32 = 1 << 20;

/// Maximum payload length we will accept off the wire (64 MiB). A 1280×800
/// 24-bit PNG is well under this; the cap bounds a corrupt `payload_len`.
const MAX_PAYLOAD_LEN: u32 = 64 << 20;

/// Write a framed message: `[u32 LE header_len][header][payload]`. The
/// caller is responsible for having set `payload_len` inside the header to
/// match `payload.len()` when serializing a [`ResponseHeader`]; for request
/// frames the payload is empty.
pub fn write_frame<W: Write>(w: &mut W, header: &[u8], payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(header.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "header too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(header)?;
    if !payload.is_empty() {
        w.write_all(payload)?;
    }
    w.flush()
}

/// Read the framed header bytes: a `u32 LE` length followed by that many
/// bytes. Does not read the payload — the caller parses the header to learn
/// `payload_len`, then calls [`read_payload`].
pub fn read_header<R: Read>(r: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_HEADER_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("header length {len} exceeds cap {MAX_HEADER_LEN}"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Read exactly `len` payload bytes (the binary tail of a response frame).
pub fn read_payload<R: Read>(r: &mut R, len: u32) -> io::Result<Vec<u8>> {
    if len > MAX_PAYLOAD_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("payload length {len} exceeds cap {MAX_PAYLOAD_LEN}"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Serialize and send an [`Action`] as a request frame (no payload).
pub fn send_action<W: Write>(w: &mut W, action: &Action) -> io::Result<()> {
    let header = serde_json::to_vec(action)?;
    write_frame(w, &header, &[])
}

/// Read a full response frame: parse the [`ResponseHeader`], then read its
/// declared `payload_len` bytes.
pub fn read_response<R: Read>(r: &mut R) -> io::Result<(ResponseHeader, Vec<u8>)> {
    let header_bytes = read_header(r)?;
    let header: ResponseHeader = serde_json::from_slice(&header_bytes)?;
    let payload = if header.payload_len > 0 {
        read_payload(r, header.payload_len)?
    } else {
        Vec::new()
    };
    Ok((header, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_header_only() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"hello", &[]).unwrap();
        // 4-byte LE length (5) + "hello"
        assert_eq!(&buf[..4], &5u32.to_le_bytes());
        assert_eq!(&buf[4..], b"hello");

        let mut cur = std::io::Cursor::new(buf);
        let header = read_header(&mut cur).unwrap();
        assert_eq!(header, b"hello");
    }

    #[test]
    fn frame_round_trip_with_payload() {
        let header = br#"{"ok":true,"payload_len":4}"#;
        let payload = [0xDE, 0xAD, 0xBE, 0xEF];
        let mut buf = Vec::new();
        write_frame(&mut buf, header, &payload).unwrap();

        let mut cur = std::io::Cursor::new(buf);
        let (h, p) = read_response(&mut cur).unwrap();
        assert!(h.ok);
        assert_eq!(h.payload_len, 4);
        assert_eq!(p, payload);
    }

    #[test]
    fn send_action_then_read_back_as_frame() {
        let mut buf = Vec::new();
        send_action(&mut buf, &Action::LeftClick).unwrap();
        let mut cur = std::io::Cursor::new(buf);
        let header = read_header(&mut cur).unwrap();
        let a: Action = serde_json::from_slice(&header).unwrap();
        assert_eq!(a, Action::LeftClick);
    }

    #[test]
    fn oversized_header_length_is_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(MAX_HEADER_LEN + 1).to_le_bytes());
        let mut cur = std::io::Cursor::new(buf);
        let err = read_header(&mut cur).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
