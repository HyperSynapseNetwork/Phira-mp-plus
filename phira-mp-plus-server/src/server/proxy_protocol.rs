//! PROXY protocol v1/v2 header parser and stream reader.
//!
//! Supports the standard HAProxy PROXY protocol:
//!
//! - **v1** (human-readable): `PROXY TCP4 192.168.1.1 10.0.0.1 12345 80\r\n`
//! - **v2** (binary): 12-byte signature + 4-byte header + address family data
//!
//! The [`maybe_read_proxy_header`] function is designed to be called inside
//! the accept pre-auth task for connections whose peer IP matches the
//! configured `proxy_allow_cidr`.  It uses `std::net::TcpStream::peek` so
//! that non-PROXY connections from the same CIDR range are **not** corrupted.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use tokio::net::TcpStream;
use tracing::warn;

// ── Constants ─────────────────────────────────────────────────────────

/// PROXY protocol v2 12-byte signature (magic bytes).
const PROXY_V2_SIG: [u8; 12] = *b"\x0D\x0A\x0D\x0A\x00\x0D\x0A\x51\x55\x49\x54\x0A";

// ── Public types ──────────────────────────────────────────────────────

/// Parsed PROXY protocol header.
///
/// Only the `Tcp4` and `Tcp6` variants carry usable source addresses.
/// `Unknown` covers `UNKNOWN` (v1), `LOCAL` (v2), Unix-family (v2),
/// and any unrecognised transport.
#[derive(Debug, Clone, PartialEq)]
pub enum ProxyHeader {
    Tcp4 { src: SocketAddr, dst: SocketAddr },
    Tcp6 { src: SocketAddr, dst: SocketAddr },
    Unix { src: String, dst: String },
    Unknown,
}

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

    // Partial v2 signature?  (unlikely in practice, but handle gracefully)
    if data.len() < 12
        && !data.is_empty()
        && PROXY_V2_SIG.starts_with(data)
    {
        return Ok(None); // need more data
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
        let _prefix: u8 = pre_str
            .parse()
            .map_err(|_| format!("invalid prefix length in CIDR \"{cidr}\""))?;
        let _network: IpAddr = net_str
            .trim()
            .parse()
            .map_err(|_| format!("invalid network address in CIDR \"{cidr}\""))?;
    }
    Ok(())
}

// ── Stream-level PROXY reading ────────────────────────────────────────

/// Attempt to detect and parse a PROXY protocol header from a new TCP
/// connection using a **peek** operation (bytes are not consumed if no
/// PROXY header is found).
///
/// **When to call this:**
/// Only for connections whose peer IP matches `proxy_allow_cidr`.  Non-PROXY
/// connections from an allowed range are handled gracefully:
/// the stream is returned without any bytes consumed.
///
/// **Performance note:**
/// Internally converts to `std::net::TcpStream` for the peek and then back
/// to `tokio::net::TcpStream`.  This involves a brief blocking read (capped
/// at 2 s via `set_read_timeout`).  Because this runs inside the already-
/// spawned pre-auth task (not the accept loop), it does not stall new
/// connection accepts.
pub async fn maybe_read_proxy_header(
    stream: TcpStream,
) -> (TcpStream, Option<SocketAddr>) {
    use std::io::Read;
    use std::time::Duration;

    // Convert to std stream so we can use `peek` (non-consuming read).
    // `into_std` consumes the tokio stream — on error we can't recover it,
    // but that shouldn't happen in practice.
    let std_stream = match stream.into_std() {
        Ok(s) => s,
        Err(e) => {
            warn!("maybe_read_proxy_header: into_std failed: {e}");
            return (TcpStream::from_std(std::net::TcpStream::connect("127.0.0.1:1")
                .expect("fallback connect for error path")), None);
        }
    };

    // Set a modest read timeout so a stuck peer doesn't block the task
    // indefinitely.
    let _ = std_stream.set_read_timeout(Some(Duration::from_secs(2)));

    // Peek at the first bytes without consuming them.
    let mut peek_buf = [0u8; 12];
    let peek_result = std_stream.peek(&mut peek_buf);

    let proxy_addr = match peek_result {
        // ── v1: "PROXY …\r\n" ────────────────────────────────────
        Ok(n) if n >= 6 && peek_buf.starts_with(b"PROXY ") => {
            match read_proxy_v1(&mut std_stream) {
                Ok(h) => proxy_source_addr(&h),
                Err(e) => {
                    warn!("PROXY v1 header read failed: {e}");
                    None
                }
            }
        }

        // ── v2: 12-byte signature ────────────────────────────────
        Ok(n) if n >= 12 && &peek_buf[..12] == &PROXY_V2_SIG => {
            match read_proxy_v2(&mut std_stream) {
                Ok(h) => proxy_source_addr(&h),
                Err(e) => {
                    warn!("PROXY v2 header read failed: {e}");
                    None
                }
            }
        }

        // Data available but does not start with a PROXY signature.
        // Peek did NOT consume anything, so the stream is untouched.
        Ok(_) => None,

        // No data available within the timeout → assume no PROXY header.
        Err(ref e)
            if e.kind() == std::io::ErrorKind::TimedOut
                || e.kind() == std::io::ErrorKind::WouldBlock =>
        {
            None
        }

        Err(e) => {
            warn!("maybe_read_proxy_header: peek error: {e}");
            None
        }
    };

    // Convert back to a tokio-managed stream.
    match tokio::net::TcpStream::from_std(std_stream) {
        Ok(ts) => (ts, proxy_addr),
        Err(e) => {
            // Extremely unlikely — only fails when the runtime cannot
            // register the fd.  If this happens the socket is lost.
            panic!(
                "maybe_read_proxy_header: failed to convert std stream \
                 back to tokio stream: {e}"
            );
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────

/// Parse a PROXY v1 header line (blocking read on `std_stream`).
///
/// The caller **must** have confirmed via peek that the stream starts with
/// `b"PROXY "` before calling this.
fn read_proxy_v1(stream: &mut std::net::TcpStream) -> Result<ProxyHeader, String> {
    use std::io::Read;

    let mut header = Vec::with_capacity(107);
    // push the prefix already confirmed by peek
    header.extend_from_slice(b"PROXY ");

    loop {
        let mut byte = [0u8; 1];
        stream
            .read_exact(&mut byte)
            .map_err(|e| format!("error reading PROXY v1 header: {e}"))?;
        header.push(byte[0]);

        if header.len() >= 2 && header[header.len() - 2..] == [b'\r', b'\n'] {
            let (hdr, _consumed) = parse_proxy_header(&header)?
                .ok_or_else(|| "failed to parse PROXY v1 header".to_string())?;
            return Ok(hdr);
        }

        if header.len() > 256 {
            return Err("PROXY v1 header exceeds 256 bytes".to_string());
        }
    }
}

/// Parse a PROXY v2 binary header (blocking read on `std_stream`).
///
/// The caller **must** have confirmed via peek that the stream starts with
/// `PROXY_V2_SIG` before calling this.
fn read_proxy_v2(stream: &mut std::net::TcpStream) -> Result<ProxyHeader, String> {
    use std::io::Read;

    // Read the 4-byte header that follows the 12-byte signature.
    let mut buf = [0u8; 16]; // sig(12) + ver_cmd(1) + fam(1) + addr_len(2)
    stream
        .read_exact(&mut buf)
        .map_err(|e| format!("error reading PROXY v2 header: {e}"))?;

    // Sanity-check the signature.
    if buf[..12] != PROXY_V2_SIG {
        return Err("PROXY v2 signature mismatch after peek".to_string());
    }

    let _ver_cmd = buf[12];
    let fam = buf[13];
    let addr_len = u16::from_be_bytes([buf[14], buf[15]]) as usize;

    // Read the variable-length address data.
    let mut addr_data = vec![0u8; addr_len];
    if addr_len > 0 {
        stream
            .read_exact(&mut addr_data)
            .map_err(|e| format!("error reading PROXY v2 address data: {e}"))?;
    }

    // Build the complete binary frame and parse it.
    let mut full = buf.to_vec();
    full.extend_from_slice(&addr_data);

    let (hdr, _consumed) = parse_v2_body(&full)?
        .ok_or_else(|| "failed to parse PROXY v2 body".to_string())?;
    Ok(hdr)
}

/// Parse a PROXY v1 header from a byte slice (already confirmed to start
/// with "PROXY ").
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
            let src_ip = Ipv6Addr::from(<[u8; 16]>::try_from(&addr_data[0..16]).unwrap());
            let dst_ip = Ipv6Addr::from(<[u8; 16]>::try_from(&addr_data[16..32]).unwrap());
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
            let mask = if prefix >= 32 {
                u32::MAX
            } else {
                u32::MAX << (32 - prefix)
            };
            (u32::from(*ip) & mask) == (u32::from(net) & mask)
        }
        (IpAddr::V6(ip), IpAddr::V6(net)) => {
            let mask = if prefix >= 128 {
                u128::MAX
            } else {
                u128::MAX << (128 - prefix)
            };
            (u128::from(*ip) & mask) == (u128::from(net) & mask)
        }
        _ => false, // address family mismatch
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── v1 ──────────────────────────────────────────────────────────

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
            ProxyHeader::Tcp6 { src, dst } => {
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

    // ── v2 ──────────────────────────────────────────────────────────

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
        assert!(parse_proxy_header(b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b").is_err());
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
        assert!(ip_matches_any_cidr(&ip, "192.168.0.0/16,10.0.0.0/8"));
        let ip: IpAddr = "172.16.0.1".parse().unwrap();
        assert!(!ip_matches_any_cidr(&ip, "192.168.0.0/16,10.0.0.0/8"));
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
}
