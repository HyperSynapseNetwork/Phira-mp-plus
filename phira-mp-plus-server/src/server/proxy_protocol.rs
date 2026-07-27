//! PROXY protocol v1/v2 header parser and pure-tokio stream reader.
//!
//! Supports the standard HAProxy PROXY protocol:
//!
//! - **v1** (human-readable): `PROXY TCP4 192.168.1.1 10.0.0.1 12345 80\r\n`
//! - **v2** (binary): 12-byte signature + 4-byte header + address family data
//!
//! # Design
//!
//! [`maybe_read_proxy_header`] is a **pure tokio async** function designed
//! to be called from the accept pre-auth task for connections whose peer IP
//! matches `proxy_allow_cidr`.  Because those connections are *expected* to
//! carry a PROXY header, the function reads header bytes directly from the
//! tokio [`TcpStream`] without needing `peek` — if the header is invalid or
//! absent the caller closes the connection (no corruption risk).
//!
//! v1 headers are read until `\r\n` (one byte at a time after the `PROXY `
//! prefix is confirmed) so the reader never consumes application-protocol
//! bytes.  v2 headers are read in three exact chunks: 12‑byte signature,
//! 4‑byte header, variable‑length address data.
//!
//! # Bugs fixed from PMP22 audit
//!
//! 1. v1 `PROXY ` prefix duplication — eliminated (v1 bytes are now read
//!    precisely once via tokio async I/O).
//! 2. `tokio::TcpStream → std::TcpStream` conversion kept `O_NONBLOCK`,
//!    causing `WouldBlock` on `read_exact` — eliminated (pure tokio).
//! 3. Error branch connected to `127.0.0.1:1` with `expect()`, causing a
//!    panic — eliminated (errors are returned cleanly via [`ProxyError`]).
//! 4. Rate‑limiting was applied to the proxy‑peer IP only — the caller now
//!    also rate‑limits the forwarded client IP after header parsing.
//! 5. No timeout on header parsing — all reads are bounded by a configurable
//!    [`Duration`].

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

// ── Constants ─────────────────────────────────────────────────────────

/// PROXY protocol v2 12‑byte signature (magic bytes).
const PROXY_V2_SIG: [u8; 12] = *b"\x0D\x0A\x0D\x0A\x00\x0D\x0A\x51\x55\x49\x54\x0A";

/// Default timeout for reading the PROXY header from a peer.
const DEFAULT_HEADER_TIMEOUT: Duration = Duration::from_secs(3);

/// Hard upper bound for any PROXY header (v1 line or v2 frame).
const MAX_HEADER_LEN: usize = 16384;

/// PROXY v1 line maximum per the spec (including `PROXY ` + `\r\n`).
const V1_MAX_LINE: usize = 108;

// ── Public types ──────────────────────────────────────────────────────

/// Parsed PROXY protocol header.
///
/// Only the `Tcp4` and `Tcp6` variants carry usable source addresses.
/// `Unknown` covers `UNKNOWN` (v1), `LOCAL` (v2), Unix‑family (v2),
/// and any unrecognised transport.
#[derive(Debug, Clone, PartialEq)]
pub enum ProxyHeader {
    Tcp4 { src: SocketAddr, dst: SocketAddr },
    Tcp6 { src: SocketAddr, dst: SocketAddr },
    Unix { src: String, dst: String },
    Unknown,
}

/// Errors that can occur while reading or parsing a PROXY protocol header.
#[derive(Debug)]
pub enum ProxyError {
    /// The configured timeout elapsed before the header was complete.
    Timeout,
    /// Underlying I/O error from tokio.
    Io(std::io::Error),
    /// The data on the wire does not match v1 or v2 signature.
    InvalidSignature,
    /// Header exceeded the configured maximum length.
    HeaderTooLarge(usize),
    /// The header bytes are syntactically invalid.
    Parse(String),
    /// Connection closed before the header could be fully read.
    Eof,
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "timeout reading PROXY header"),
            Self::Io(e) => write!(f, "I/O error reading PROXY header: {e}"),
            Self::InvalidSignature => {
                write!(f, "data does not match PROXY v1 or v2 signature")
            }
            Self::HeaderTooLarge(n) => {
                write!(f, "PROXY header {n} bytes exceeds allowed maximum")
            }
            Self::Parse(e) => write!(f, "PROXY header parse error: {e}"),
            Self::Eof => write!(f, "connection closed before PROXY header complete"),
        }
    }
}

impl std::error::Error for ProxyError {}

// ── Public helpers ────────────────────────────────────────────────────

/// Extract the source (client) address from a PROXY header, if applicable.
pub fn proxy_source_addr(header: &ProxyHeader) -> Option<SocketAddr> {
    match header {
        ProxyHeader::Tcp4 { src, .. } | ProxyHeader::Tcp6 { src, .. } => Some(*src),
        _ => None,
    }
}

/// Parse a PROXY protocol v1 or v2 header from raw bytes.
///
/// Returns:
/// - `Ok(Some((header, bytes_consumed)))` on success.
/// - `Ok(None)` when more data is needed (incomplete header).
/// - `Err(reason)` on invalid or unrecognised data.
///
/// `bytes_consumed` is the number of bytes from `data` that belong to the
/// header (including the terminator for v1 or the full binary frame for v2).
pub fn parse_proxy_header(data: &[u8]) -> Result<Option<(ProxyHeader, usize)>, String> {
    // v1 starts with "PROXY "
    if data.starts_with(b"PROXY ") {
        return parse_v1_header(data);
    }

    // v2 starts with a 12-byte fixed signature
    if data.len() >= 12 && data[..12] == PROXY_V2_SIG {
        return parse_v2_body(data);
    }

    // Partial v2 signature — need more data.
    if data.len() < 12 && !data.is_empty() && PROXY_V2_SIG.starts_with(data) {
        return Ok(None);
    }

    // Data is present but does not match any known PROXY signature.
    Err("data does not start with a valid PROXY protocol signature".to_string())
}

// ── CIDR matching ─────────────────────────────────────────────────────

/// Check whether `ip` belongs to any CIDR in a comma-separated list.
///
/// Example: `"10.0.0.0/8,192.168.0.0/16"`
pub fn ip_matches_any_cidr(ip: &IpAddr, cidr_list: &str) -> bool {
    cidr_list.split(',').any(|cidr| {
        let cidr = cidr.trim();
        if cidr.is_empty() {
            return false;
        }
        ip_matches_single_cidr(ip, cidr)
    })
}

/// Validate that a comma-separated CIDR list is syntactically correct.
pub fn validate_cidr_list(value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("proxy_allow_cidr must not be empty when set".to_string());
    }
    for part in trimmed.split(',') {
        let cidr = part.trim();
        if cidr.is_empty() {
            continue;
        }
        let (net_str, pre_str) = cidr
            .split_once('/')
            .ok_or_else(|| format!("invalid CIDR \"{cidr}\": missing prefix length"))?;
        let prefix: u8 = pre_str
            .parse()
            .map_err(|_| format!("invalid prefix length in CIDR \"{cidr}\""))?;
        let network: IpAddr = net_str
            .trim()
            .parse()
            .map_err(|_| format!("invalid network address in CIDR \"{cidr}\""))?;
        let max_prefix = match network {
            IpAddr::V4(_) => 32u8,
            IpAddr::V6(_) => 128u8,
        };
        if prefix > max_prefix {
            return Err(format!(
                "prefix length {prefix} exceeds maximum {max_prefix} for CIDR \"{cidr}\""
            ));
        }
    }
    Ok(())
}

// ── Stream‑level PROXY reading (pure tokio async) ─────────────────────

/// Read a PROXY protocol header from a tokio [`TcpStream`] without using
/// `std::net::TcpStream` conversion or blocking I/O.
///
/// # When to call
///
/// Only for connections whose peer IP matches `proxy_allow_cidr`.
/// Non‑PROXY connections from an allowed range are **not** handled
/// gracefully — the function returns `Err` and the caller **must** close
/// the connection (the admin has opted the CIDR in for PROXY, so a missing
/// header is a protocol violation).
///
/// # How it works
///
/// 1. Read the first byte to distinguish v1 (`P`) from v2 (`\r = 0x0D`).
/// 2. **v1** — confirm `ROXY ` (5 B), then read one byte at a time until
///    `\r\n` is seen.  The next byte in the kernel buffer is the first
///    application‑protocol byte.  No byte replay is needed.
/// 3. **v2** — read remaining 11 B of signature, 4‑byte header, and
///    `addr_len` variable‑length address data.  Again no over‑read.
/// 4. Every read is bounded by `timeout` (floored at 1 s).
///
/// # Errors
///
/// Returns [`ProxyError`] on timeout, I/O error, invalid signature, or
/// parse failure.  The caller should log and close the connection.
pub async fn maybe_read_proxy_header(
    stream: TcpStream,
    timeout: Duration,
    max_header_len: usize,
) -> Result<(TcpStream, Option<SocketAddr>), ProxyError> {
    let timeout = timeout.max(Duration::from_secs(1)); // floor 1s

    // ── Peek at the first byte ─────────────────────────────────────
    let mut first = [0u8; 1];
    tokio::time::timeout(timeout, stream.read_exact(&mut first))
        .await
        .map_err(|_| ProxyError::Timeout)?
        .map_err(ProxyError::Io)?;

    match first[0] {
        // ═══════════════════════════════════════════════════════════
        // v2: starts with \r (0x0D)
        // ═══════════════════════════════════════════════════════════
        0x0D => {
            // Read remaining 11 bytes of the 12‑byte signature.
            let mut sig_rest = [0u8; 11];
            tokio::time::timeout(timeout, stream.read_exact(&mut sig_rest))
                .await
                .map_err(|_| ProxyError::Timeout)?
                .map_err(ProxyError::Io)?;

            // Reconstruct the full signature.
            let mut sig = [0u8; 12];
            sig[0] = 0x0D;
            sig[1..].copy_from_slice(&sig_rest);

            if sig != PROXY_V2_SIG {
                return Err(ProxyError::InvalidSignature);
            }

            // Read the 4‑byte v2 header (ver_cmd + fam + addr_len).
            let mut hdr = [0u8; 4];
            tokio::time::timeout(timeout, stream.read_exact(&mut hdr))
                .await
                .map_err(|_| ProxyError::Timeout)?
                .map_err(ProxyError::Io)?;

            let addr_len = u16::from_be_bytes([hdr[2], hdr[3]]) as usize;
            let total_v2 = 16 + addr_len; // sig(12) + hdr(4) + addr_data

            if total_v2 > max_header_len {
                return Err(ProxyError::HeaderTooLarge(total_v2));
            }

            // Read the variable‑length address data.
            let mut addr_data = vec![0u8; addr_len];
            if addr_len > 0 {
                tokio::time::timeout(timeout, stream.read_exact(&mut addr_data))
                    .await
                    .map_err(|_| ProxyError::Timeout)?
                    .map_err(ProxyError::Io)?;
            }

            // Reconstruct the full binary frame and parse it.
            let mut full = Vec::with_capacity(total_v2);
            full.extend_from_slice(&sig);
            full.extend_from_slice(&hdr);
            full.extend_from_slice(&addr_data);

            let (hdr, _consumed) = parse_v2_body(&full)
                .map_err(ProxyError::Parse)?
                .ok_or(ProxyError::Parse("incomplete v2 body after full read".into()))?;

            Ok((stream, proxy_source_addr(&hdr)))
        }

        // ═══════════════════════════════════════════════════════════
        // v1: starts with 'P'
        // ═══════════════════════════════════════════════════════════
        b'P' => {
            // Confirm the remaining "ROXY " prefix.
            let mut rest = [0u8; 5];
            tokio::time::timeout(timeout, stream.read_exact(&mut rest))
                .await
                .map_err(|_| ProxyError::Timeout)?
                .map_err(ProxyError::Io)?;

            if &rest != b"ROXY " {
                return Err(ProxyError::InvalidSignature);
            }

            // Accumulate the full v1 line including the prefix.
            let mut line = Vec::with_capacity(V1_MAX_LINE);
            line.push(b'P');
            line.extend_from_slice(&rest);

            // Read one byte at a time until `\r\n`.
            loop {
                if line.len() >= max_header_len {
                    return Err(ProxyError::HeaderTooLarge(line.len()));
                }

                let mut byte = [0u8; 1];
                tokio::time::timeout(timeout, stream.read_exact(&mut byte))
                    .await
                    .map_err(|_| ProxyError::Timeout)?
                    .map_err(ProxyError::Io)?;

                line.push(byte[0]);

                // Check for the `\r\n` terminator.
                if line.len() >= 2 && line[line.len() - 2..] == [b'\r', b'\n'] {
                    break;
                }
            }

            let (hdr, _consumed) = parse_v1_header(&line)
                .map_err(ProxyError::Parse)?
                .ok_or(ProxyError::Parse("incomplete v1 header after full read".into()))?;

            Ok((stream, proxy_source_addr(&hdr)))
        }

        // ═══════════════════════════════════════════════════════════
        // Neither v1 nor v2.
        // ═══════════════════════════════════════════════════════════
        _ => Err(ProxyError::InvalidSignature),
    }
}

/// Convenience wrapper that uses default timeout (3s) and max header
/// length (16 KiB).
#[allow(dead_code)]
pub async fn maybe_read_proxy_header_default(
    stream: TcpStream,
) -> Result<(TcpStream, Option<SocketAddr>), ProxyError> {
    maybe_read_proxy_header(stream, DEFAULT_HEADER_TIMEOUT, MAX_HEADER_LEN).await
}

// ── Internal helpers ──────────────────────────────────────────────────

/// Parse a PROXY v1 header from a byte slice (already confirmed to start
/// with `"PROXY "`).
fn parse_v1_header(data: &[u8]) -> Result<Option<(ProxyHeader, usize)>, String> {
    // Locate the \r\n terminator.
    let end = data
        .windows(2)
        .position(|w| w == b"\r\n")
        .map(|pos| pos + 2);

    let end = match end {
        Some(p) => p,
        None => return Ok(None), // incomplete – need more data
    };

    let line = std::str::from_utf8(&data[..end - 2])
        .map_err(|_| "PROXY v1 header is not valid UTF-8".to_string())?;

    let parts: Vec<&str> = line.split_whitespace().collect();
    // Minimum: "PROXY UNKNOWN\r\n" → 2 tokens
    if parts.len() < 2 {
        return Err("truncated PROXY v1 header".to_string());
    }

    match parts[1] {
        "TCP4" => {
            if parts.len() != 6 {
                return Err(format!(
                    "PROXY TCP4 requires 6 fields, got {}",
                    parts.len()
                ));
            }
            let src_ip: Ipv4Addr = parts[2]
                .parse()
                .map_err(|_| format!("invalid source IPv4: {}", parts[2]))?;
            let dst_ip: Ipv4Addr = parts[3]
                .parse()
                .map_err(|_| format!("invalid destination IPv4: {}", parts[3]))?;
            let src_port: u16 = parts[4]
                .parse()
                .map_err(|_| format!("invalid source port: {}", parts[4]))?;
            let dst_port: u16 = parts[5]
                .parse()
                .map_err(|_| format!("invalid destination port: {}", parts[5]))?;

            Ok(Some((
                ProxyHeader::Tcp4 {
                    src: SocketAddr::new(IpAddr::V4(src_ip), src_port),
                    dst: SocketAddr::new(IpAddr::V4(dst_ip), dst_port),
                },
                end,
            )))
        }
        "TCP6" => {
            if parts.len() != 6 {
                return Err(format!(
                    "PROXY TCP6 requires 6 fields, got {}",
                    parts.len()
                ));
            }
            let src_ip: Ipv6Addr = parts[2]
                .parse()
                .map_err(|_| format!("invalid source IPv6: {}", parts[2]))?;
            let dst_ip: Ipv6Addr = parts[3]
                .parse()
                .map_err(|_| format!("invalid destination IPv6: {}", parts[3]))?;
            let src_port: u16 = parts[4]
                .parse()
                .map_err(|_| format!("invalid source port: {}", parts[4]))?;
            let dst_port: u16 = parts[5]
                .parse()
                .map_err(|_| format!("invalid destination port: {}", parts[5]))?;

            Ok(Some((
                ProxyHeader::Tcp6 {
                    src: SocketAddr::new(IpAddr::V6(src_ip), src_port),
                    dst: SocketAddr::new(IpAddr::V6(dst_ip), dst_port),
                },
                end,
            )))
        }
        "UNKNOWN" => Ok(Some((ProxyHeader::Unknown, end))),
        other => Err(format!("unknown PROXY v1 transport type: {other}")),
    }
}

/// Parse a PROXY v2 binary header from a byte slice (already confirmed to
/// start with the 12-byte signature).
fn parse_v2_body(data: &[u8]) -> Result<Option<(ProxyHeader, usize)>, String> {
    if data.len() < 16 {
        return Ok(None); // need the 4-byte header too
    }

    let ver_cmd = data[12];
    let fam = data[13];
    let addr_len = u16::from_be_bytes([data[14], data[15]]) as usize;
    let total = 16 + addr_len;

    if data.len() < total {
        return Ok(None); // need the address data
    }

    let addr_data = &data[16..total];

    // LOCAL command (0x00) — used for health checks, no address info.
    let cmd = ver_cmd & 0x0F;
    if cmd == 0x00 {
        return Ok(Some((ProxyHeader::Unknown, total)));
    }

    // The upper nibble should be 0x20 (v2).  We do not enforce it strictly;
    // parsing proceeds based on the family byte.

    let header = match fam {
        // 0x11 = TCP over IPv4
        0x11 => {
            if addr_data.len() < 12 {
                return Err("truncated PROXY v2 TCP4 address data".to_string());
            }
            let src_ip = Ipv4Addr::new(
                addr_data[0], addr_data[1], addr_data[2], addr_data[3],
            );
            let dst_ip = Ipv4Addr::new(
                addr_data[4], addr_data[5], addr_data[6], addr_data[7],
            );
            let src_port = u16::from_be_bytes([addr_data[8], addr_data[9]]);
            let dst_port = u16::from_be_bytes([addr_data[10], addr_data[11]]);
            ProxyHeader::Tcp4 {
                src: SocketAddr::new(IpAddr::V4(src_ip), src_port),
                dst: SocketAddr::new(IpAddr::V4(dst_ip), dst_port),
            }
        }
        // 0x21 = TCP over IPv6
        0x21 => {
            if addr_data.len() < 36 {
                return Err("truncated PROXY v2 TCP6 address data".to_string());
            }
            let src_ip =
                Ipv6Addr::from(<[u8; 16]>::try_from(&addr_data[0..16]).unwrap());
            let dst_ip =
                Ipv6Addr::from(<[u8; 16]>::try_from(&addr_data[16..32]).unwrap());
            let src_port = u16::from_be_bytes([addr_data[32], addr_data[33]]);
            let dst_port = u16::from_be_bytes([addr_data[34], addr_data[35]]);
            ProxyHeader::Tcp6 {
                src: SocketAddr::new(IpAddr::V6(src_ip), src_port),
                dst: SocketAddr::new(IpAddr::V6(dst_ip), dst_port),
            }
        }
        // 0x31 = Unix stream, 0x32 = Unix dgram
        0x31 | 0x32 => ProxyHeader::Unknown,
        // Everything else (UDP, unsolicited, …)
        _ => ProxyHeader::Unknown,
    };

    Ok(Some((header, total)))
}

/// Check whether a single IP belongs to a single CIDR.
fn ip_matches_single_cidr(ip: &IpAddr, cidr: &str) -> bool {
    let (net_str, pre_str) = match cidr.split_once('/') {
        Some(pair) => pair,
        None => return false,
    };

    let prefix: u8 = match pre_str.parse() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let network: IpAddr = match net_str.trim().parse() {
        Ok(ip) => ip,
        Err(_) => return false,
    };

    match (ip, network) {
        (IpAddr::V4(ip), IpAddr::V4(net)) => {
            let shift = 32u32.saturating_sub(prefix as u32);
            let mask = u32::MAX.checked_shl(shift).unwrap_or(0);
            (u32::from(*ip) & mask) == (u32::from(net) & mask)
        }
        (IpAddr::V6(ip), IpAddr::V6(net)) => {
            let shift = 128u32.saturating_sub(prefix as u32);
            let mask = u128::MAX.checked_shl(shift).unwrap_or(0);
            (u128::from(*ip) & mask) == (u128::from(net) & mask)
        }
        _ => false, // address family mismatch
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── v1 parse (stateless) ────────────────────────────────────────

    #[test]
    fn v1_tcp4_ok() {
        let raw = b"PROXY TCP4 192.168.1.1 10.0.0.1 12345 80\r\n";
        let (hdr, n) = parse_proxy_header(raw).unwrap().unwrap();
        assert_eq!(n, raw.len());
        match hdr {
            ProxyHeader::Tcp4 { src, dst } => {
                assert_eq!(src, "192.168.1.1:12345".parse::<SocketAddr>().unwrap());
                assert_eq!(dst, "10.0.0.1:80".parse::<SocketAddr>().unwrap());
            }
            _ => panic!("expected Tcp4"),
        }
    }

    #[test]
    fn v1_tcp6_ok() {
        let raw = b"PROXY TCP6 ::1 ::ffff:127.0.0.1 65535 443\r\n";
        let (hdr, n) = parse_proxy_header(raw).unwrap().unwrap();
        assert_eq!(n, raw.len());
        match hdr {
            ProxyHeader::Tcp6 { src, dst: _ } => {
                assert_eq!(src, "[::1]:65535".parse::<SocketAddr>().unwrap());
            }
            _ => panic!("expected Tcp6"),
        }
    }

    #[test]
    fn v1_unknown() {
        let raw = b"PROXY UNKNOWN\r\n";
        let (hdr, n) = parse_proxy_header(raw).unwrap().unwrap();
        assert_eq!(n, raw.len());
        assert_eq!(hdr, ProxyHeader::Unknown);
    }

    #[test]
    fn v1_incomplete_returns_none() {
        let raw = b"PROXY TCP4 192";
        assert!(parse_proxy_header(raw).unwrap().is_none());
    }

    #[test]
    fn v1_non_utf8_rejected() {
        let raw = b"PROXY TCP4 \xff\xfe 10.0.0.1 80 443\r\n";
        assert!(parse_proxy_header(raw).is_err());
    }

    // ── v2 parse (stateless) ────────────────────────────────────────

    #[test]
    fn v2_tcp4_ok() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&PROXY_V2_SIG); // 12 bytes
        raw.push(0x21); // ver_cmd: v2 + PROXY
        raw.push(0x11); // fam: TCP4
        raw.extend_from_slice(&12u16.to_be_bytes()); // addr length
        // address data: src(4) + dst(4) + src_port(2) + dst_port(2) = 12
        raw.extend_from_slice(&[10, 0, 0, 1]); // src
        raw.extend_from_slice(&[192, 168, 1, 1]); // dst
        raw.extend_from_slice(&80u16.to_be_bytes()); // src port
        raw.extend_from_slice(&443u16.to_be_bytes()); // dst port

        let (hdr, n) = parse_proxy_header(&raw).unwrap().unwrap();
        assert_eq!(n, raw.len());
        match hdr {
            ProxyHeader::Tcp4 { src, dst } => {
                assert_eq!(src, "10.0.0.1:80".parse::<SocketAddr>().unwrap());
                assert_eq!(dst, "192.168.1.1:443".parse::<SocketAddr>().unwrap());
            }
            _ => panic!("expected Tcp4"),
        }
    }

    #[test]
    fn v2_tcp6_ok() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&PROXY_V2_SIG);
        raw.push(0x21);
        raw.push(0x21); // fam: TCP6
        raw.extend_from_slice(&36u16.to_be_bytes()); // addr length
        // src IPv6 (16) + dst IPv6 (16) + src_port (2) + dst_port (2)
        raw.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]); // ::1
        raw.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]); // ::1
        raw.extend_from_slice(&65535u16.to_be_bytes());
        raw.extend_from_slice(&80u16.to_be_bytes());

        let (hdr, n) = parse_proxy_header(&raw).unwrap().unwrap();
        assert_eq!(n, raw.len());
        match hdr {
            ProxyHeader::Tcp6 { src, .. } => {
                assert_eq!(src, "[::1]:65535".parse::<SocketAddr>().unwrap());
            }
            _ => panic!("expected Tcp6"),
        }
    }

    #[test]
    fn v2_local() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&PROXY_V2_SIG);
        raw.push(0x20); // LOCAL command
        raw.push(0x00);
        raw.extend_from_slice(&0u16.to_be_bytes()); // zero length

        let (hdr, n) = parse_proxy_header(&raw).unwrap().unwrap();
        assert_eq!(n, raw.len());
        assert_eq!(hdr, ProxyHeader::Unknown);
    }

    #[test]
    fn v2_incomplete_header_returns_none() {
        let raw = &PROXY_V2_SIG[..8]; // only 8 of 12 signature bytes
        assert!(parse_proxy_header(raw).unwrap().is_none());
    }

    #[test]
    fn v2_missing_address_data_returns_none() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&PROXY_V2_SIG);
        raw.push(0x21);
        raw.push(0x11);
        raw.extend_from_slice(&12u16.to_be_bytes()); // claims 12 address bytes
        // but we only supply 4
        raw.extend_from_slice(&[10, 0, 0, 1]);
        assert!(parse_proxy_header(&raw).unwrap().is_none());
    }

    // ── Random data is rejected ─────────────────────────────────────

    #[test]
    fn random_bytes_rejected() {
        assert!(parse_proxy_header(b"GET / HTTP/1.1\r\n").is_err());
        assert!(parse_proxy_header(b"SSH-2.0-OpenSSH").is_err());
        assert!(
            parse_proxy_header(b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b").is_err()
        );
    }

    // ── CIDR ────────────────────────────────────────────────────────

    #[test]
    fn cidr_ipv4_match() {
        let ip: IpAddr = "10.0.0.5".parse().unwrap();
        assert!(ip_matches_any_cidr(&ip, "10.0.0.0/8"));
        assert!(ip_matches_any_cidr(&ip, "0.0.0.0/0"));
        assert!(!ip_matches_any_cidr(&ip, "192.168.0.0/16"));
    }

    #[test]
    fn cidr_ipv6_match() {
        let ip: IpAddr = "fd00::1".parse().unwrap();
        assert!(ip_matches_any_cidr(&ip, "fd00::/8"));
        assert!(!ip_matches_any_cidr(&ip, "fe80::/10"));
    }

    #[test]
    fn cidr_list_multiple() {
        let ip: IpAddr = "10.0.0.5".parse().unwrap();
        assert!(
            ip_matches_any_cidr(&ip, "192.168.0.0/16,10.0.0.0/8")
        );
        let ip: IpAddr = "172.16.0.1".parse().unwrap();
        assert!(
            !ip_matches_any_cidr(&ip, "192.168.0.0/16,10.0.0.0/8")
        );
    }

    #[test]
    fn cidr_mismatched_family() {
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(!ip_matches_any_cidr(&ip, "::/0"));
    }

    #[test]
    fn validate_good_cidr() {
        assert!(validate_cidr_list("10.0.0.0/8").is_ok());
        assert!(validate_cidr_list("10.0.0.0/8,192.168.0.0/16").is_ok());
        assert!(validate_cidr_list("::/0").is_ok());
    }

    #[test]
    fn validate_bad_cidr() {
        assert!(validate_cidr_list("10.0.0.0/33").is_err());
        assert!(validate_cidr_list("not-a-cidr").is_err());
        assert!(validate_cidr_list("").is_err());
    }

    #[test]
    fn proxy_source_addr_from_tcp4() {
        let hdr = ProxyHeader::Tcp4 {
            src: "1.2.3.4:5678".parse().unwrap(),
            dst: "10.0.0.1:80".parse().unwrap(),
        };
        assert_eq!(
            proxy_source_addr(&hdr),
            Some("1.2.3.4:5678".parse().unwrap())
        );
    }

    #[test]
    fn proxy_source_addr_unknown() {
        assert_eq!(proxy_source_addr(&ProxyHeader::Unknown), None);
    }

    // ── Integration tests (real tokio TcpStream) ────────────────────
    //
    // These exercise the full `maybe_read_proxy_header` path: a peer
    // connects and sends PROXY header bytes, and the parser reads them
    // from the tokio TcpStream.

    #[tokio::test]
    async fn integration_v1_complete() {
        let payload = b"PROXY TCP4 192.168.1.1 10.0.0.1 12345 80\r\n";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();

        let jh = tokio::spawn(async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.writable().await.unwrap();
            let _ = s.try_write(payload);
            // Keep the connection alive so the reader can complete.
            tokio::time::sleep(Duration::from_millis(500)).await;
        });

        let (stream, _) = listener.accept().await.unwrap();
        let (_stream, proxy_addr) =
            maybe_read_proxy_header(stream, Duration::from_secs(3), MAX_HEADER_LEN)
                .await
                .unwrap();
        assert!(proxy_addr.is_some());
        let addr = proxy_addr.unwrap();
        assert_eq!(
            addr,
            "192.168.1.1:12345".parse::<SocketAddr>().unwrap()
        );

        jh.await.unwrap();
    }

    #[tokio::test]
    async fn integration_v2_complete() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&PROXY_V2_SIG);
        raw.push(0x21);
        raw.push(0x11);
        raw.extend_from_slice(&12u16.to_be_bytes());
        raw.extend_from_slice(&[10, 0, 0, 1]);
        raw.extend_from_slice(&[192, 168, 1, 1]);
        raw.extend_from_slice(&80u16.to_be_bytes());
        raw.extend_from_slice(&443u16.to_be_bytes());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();

        let jh = tokio::spawn(async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.writable().await.unwrap();
            let _ = s.try_write(&raw);
            tokio::time::sleep(Duration::from_millis(500)).await;
        });

        let (stream, _) = listener.accept().await.unwrap();
        let (_stream, proxy_addr) =
            maybe_read_proxy_header(stream, Duration::from_secs(3), MAX_HEADER_LEN)
                .await
                .unwrap();
        assert!(proxy_addr.is_some());
        let addr = proxy_addr.unwrap();
        assert_eq!(addr, "10.0.0.1:80".parse::<SocketAddr>().unwrap());

        jh.await.unwrap();
    }

    #[tokio::test]
    async fn integration_no_proxy_http() {
        // A plain HTTP request from a trusted CIDR === protocol violation.
        let payload = b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();

        let jh = tokio::spawn(async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.writable().await.unwrap();
            let _ = s.try_write(payload);
            tokio::time::sleep(Duration::from_millis(500)).await;
        });

        let (stream, _) = listener.accept().await.unwrap();
        let result =
            maybe_read_proxy_header(stream, Duration::from_secs(3), MAX_HEADER_LEN).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ProxyError::InvalidSignature => {} // expected
            e => panic!("expected InvalidSignature, got {e:?}"),
        }

        jh.await.unwrap();
    }

    #[tokio::test]
    async fn integration_no_proxy_tls() {
        // TLS ClientHello from a trusted CIDR === protocol violation.
        let payload = b"\x16\x03\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();

        let jh = tokio::spawn(async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.writable().await.unwrap();
            let _ = s.try_write(payload);
            tokio::time::sleep(Duration::from_millis(500)).await;
        });

        let (stream, _) = listener.accept().await.unwrap();
        let result =
            maybe_read_proxy_header(stream, Duration::from_secs(3), MAX_HEADER_LEN).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ProxyError::InvalidSignature => {} // expected
            e => panic!("expected InvalidSignature, got {e:?}"),
        }

        jh.await.unwrap();
    }

    #[tokio::test]
    async fn integration_invalid_v2_sig() {
        // 12 bytes where first byte matches v2 but the rest don't.
        let mut raw = Vec::new();
        raw.push(0x0D); // matches v2
        raw.extend_from_slice(b"AAAAAAAAAAA"); // 11 bytes that aren't the real sig

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();

        let jh = tokio::spawn(async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.writable().await.unwrap();
            let _ = s.try_write(&raw);
            tokio::time::sleep(Duration::from_millis(500)).await;
        });

        let (stream, _) = listener.accept().await.unwrap();
        let result =
            maybe_read_proxy_header(stream, Duration::from_secs(3), MAX_HEADER_LEN).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ProxyError::InvalidSignature => {} // expected
            e => panic!("expected InvalidSignature, got {e:?}"),
        }

        jh.await.unwrap();
    }

    #[tokio::test]
    async fn integration_v1_partial_timeout() {
        // Send only "PROXY TCP4 " and then stop — the reader should time
        // out waiting for the rest.
        let payload = b"PROXY TCP4 ";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();

        let jh = tokio::spawn(async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.writable().await.unwrap();
            let _ = s.try_write(payload);
            // Hold the connection open without sending more data.
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let (stream, _) = listener.accept().await.unwrap();
        // Use a short timeout so the test finishes quickly.
        let result =
            maybe_read_proxy_header(stream, Duration::from_millis(200), MAX_HEADER_LEN).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ProxyError::Timeout => {} // expected
            e => panic!("expected Timeout, got {e:?}"),
        }

        jh.await.unwrap();
    }

    #[tokio::test]
    async fn integration_v2_partial_timeout() {
        // Send only the v2 signature (12 bytes) without the 4-byte header.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();

        let jh = tokio::spawn(async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.writable().await.unwrap();
            let _ = s.try_write(&PROXY_V2_SIG);
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let (stream, _) = listener.accept().await.unwrap();
        let result =
            maybe_read_proxy_header(stream, Duration::from_millis(200), MAX_HEADER_LEN).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ProxyError::Timeout => {} // expected
            e => panic!("expected Timeout, got {e:?}"),
        }

        jh.await.unwrap();
    }

    #[tokio::test]
    async fn integration_max_header_len_respected() {
        // Send a v2 header with an addr_len that would exceed the limit
        // (65535 > our max_header_len of 100).
        let mut raw = Vec::new();
        raw.extend_from_slice(&PROXY_V2_SIG);
        raw.push(0x21);
        raw.push(0x11);
        raw.extend_from_slice(&0xFFFFu16.to_be_bytes()); // addr_len = 65535

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();

        let jh = tokio::spawn(async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.writable().await.unwrap();
            let _ = s.try_write(&raw);
            tokio::time::sleep(Duration::from_millis(500)).await;
        });

        let (stream, _) = listener.accept().await.unwrap();
        let result = maybe_read_proxy_header(stream, Duration::from_secs(3), 100).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ProxyError::HeaderTooLarge(_) => {} // expected
            e => panic!("expected HeaderTooLarge, got {e:?}"),
        }

        jh.await.unwrap();
    }

    /// Verify that the stream is still usable after reading a valid PROXY
    /// header — application data that follows in the same TCP segment
    /// must be readable from the returned TcpStream.
    #[tokio::test]
    async fn integration_remaining_data_readable() {
        let proxy_hdr = b"PROXY TCP4 10.0.0.1 20.0.0.1 12345 80\r\n";
        let app_data = b"HELLO";
        let mut full = Vec::new();
        full.extend_from_slice(proxy_hdr);
        full.extend_from_slice(app_data);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();

        let jh = tokio::spawn(async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.writable().await.unwrap();
            // Send both PROXY header and app data in one write.
            let _ = s.try_write(&full);
            tokio::time::sleep(Duration::from_millis(500)).await;
        });

        let (stream, _) = listener.accept().await.unwrap();
        let (mut stream, proxy_addr) =
            maybe_read_proxy_header(stream, Duration::from_secs(3), MAX_HEADER_LEN)
                .await
                .unwrap();
        assert!(proxy_addr.is_some());

        // The app data must still be readable from the returned stream.
        // It was sent in the same TCP segment but the PROXY header reader
        // consumed exactly the v1 header bytes, leaving "HELLO" untouched.
        let mut buf = [0u8; 5];
        stream.readable().await.unwrap();
        let n = stream.try_read(&mut buf).unwrap();
        assert_eq!(&buf[..n], app_data);

        jh.await.unwrap();
    }
}
