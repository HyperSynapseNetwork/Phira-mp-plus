//! PROXY protocol v1/v2 parser.
//!
//! Implements parsing of the PROXY protocol header (both v1 text and v2 binary)
//! to extract the real client IP address when the server is behind a reverse
//! proxy such as HAProxy, Nginx, or Cloudflare.
//!
//! # Integration
//! Call [`accept_proxy_or_direct`] right after accepting a TCP connection.
//! It uses `TcpStream::peek` so non-PROXY connections are untouched.
//!
//! The returned `TcpStream` has the PROXY header consumed (bytes removed
//! from the stream), so it can be passed directly to hyper / axum.
//!
//! ```ignore
//! use tokio::net::TcpListener;
//! use phira_mp_plus_server::proxy_protocol::accept_proxy_or_direct;
//!
//! let listener = TcpListener::bind("0.0.0.0:8080").await?;
//! loop {
//!     let (stream, peer_addr) = listener.accept().await?;
//!     let (mut stream, real_ip) = accept_proxy_or_direct(stream).await;
//!     // use real_ip.unwrap_or(peer_addr.ip()) as the client IP
//! }
//! ```

use std::io::Cursor;
use std::net::{IpAddr, SocketAddr};

/// Result of parsing a PROXY protocol header.
#[derive(Debug, Clone)]
pub struct ProxyHeader {
    /// The real source (client) address.
    pub source: SocketAddr,
    /// The destination address the original connection was addressed to.
    pub destination: SocketAddr,
    /// Whether the protocol is v2 (binary) or v1 (text).
    pub is_v2: bool,
}

/// Accept a TCP connection and optionally parse a PROXY protocol header.
///
/// Uses `TcpStream::peek` to detect the PROXY signature without consuming
/// bytes, so if the connection is direct (HTTP without PROXY), the stream
/// is returned unchanged.
///
/// Returns `(stream, Some(real_ip))` on PROXY success,
/// `(stream, None)` for direct connections.
pub async fn accept_proxy_or_direct(
    stream: tokio::net::TcpStream,
) -> (tokio::net::TcpStream, Option<IpAddr>) {
    // Peek at the first 4 bytes — this does NOT consume them.
    let mut peek_buf = [0u8; 4];
    let n = match stream.peek(&mut peek_buf).await {
        Ok(n) => n,
        Err(_) => return (stream, None),
    };
    if n < 4 {
        return (stream, None);
    }

    if peek_buf[0] == b'P' && peek_buf[1] == b'R' && peek_buf[2] == b'O' && peek_buf[3] == b'X' {
        // PROXY v1 — consume the header and parse.
        match parse_v1_from_stream(&stream).await {
            Ok(hdr) => (stream, Some(hdr.source.ip())),
            Err(_) => (stream, None),
        }
    } else if peek_buf[0] == 0x0D && peek_buf[1] == 0x0A
        && peek_buf[2] == 0x0D && peek_buf[3] == 0x0A
    {
        // PROXY v2.
        match parse_v2_from_stream(&stream).await {
            Ok(hdr) => (stream, Some(hdr.source.ip())),
            Err(_) => (stream, None),
        }
    } else {
        // Direct HTTP connection — stream is untouched.
        (stream, None)
    }
}

/// Read and parse a PROXY v1 text header from the stream.
///
/// `peek` confirmed the stream starts with "PROXY"; we now read the
/// full line including the trailing \n (consuming it from the stream).
async fn parse_v1_from_stream(stream: &tokio::net::TcpStream) -> std::io::Result<ProxyHeader> {
    let mut buf = Vec::with_capacity(128);
    let mut tmp = [0u8; 1];
    loop {
        stream.readable().await?;
        match stream.try_read(&mut tmp) {
            Ok(0) => break,
            Ok(1) => {
                if tmp[0] == b'\n' {
                    break;
                }
                buf.push(tmp[0]);
                if buf.len() > 256 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "PROXY v1: header too long",
                    ));
                }
            }
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }

    let line = String::from_utf8_lossy(&buf);
    let trimmed = line.trim_end_matches('\r');
    let parts: Vec<&str> = trimmed.split_whitespace().collect();

    match parts.as_slice() {
        ["PROXY", "TCP4", src_ip, dst_ip, src_port, dst_port]
        | ["PROXY", "TCP6", src_ip, dst_ip, src_port, dst_port] => {
            let src: IpAddr = src_ip.parse().map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, e)
            })?;
            let sport: u16 = src_port.parse().map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, e)
            })?;
            Ok(ProxyHeader {
                source: SocketAddr::new(src, sport),
                destination: SocketAddr::new(dst_ip.parse().unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)), dst_port.parse().unwrap_or(0)),
                is_v2: false,
            })
        }
        ["PROXY", "UNKNOWN", ..] => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "PROXY UNKNOWN",
        )),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("PROXY v1: malformed: {trimmed}"),
        )),
    }
}

/// Read and parse a PROXY v2 binary header from the stream.
async fn parse_v2_from_stream(stream: &tokio::net::TcpStream) -> std::io::Result<ProxyHeader> {
    // Read remaining 8 signature bytes (first 4 were confirmed by peek).
    let mut sig_remain = [0u8; 8];
    read_exact_from_stream(stream, &mut sig_remain).await?;

    let expected: [u8; 8] = [0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A];
    if sig_remain != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "PROXY v2: invalid signature trailer",
        ));
    }

    // Read 4-byte header: version/command, family, address length
    let mut hdr = [0u8; 4];
    read_exact_from_stream(stream, &mut hdr).await?;
    let version_cmd = hdr[0];
    let family = hdr[1];
    let addr_len = u16::from_be_bytes([hdr[2], hdr[3]]) as usize;

    if version_cmd & 0xF0 != 0x20 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("PROXY v2: unsupported version 0x{version_cmd:02x}"),
        ));
    }

    let is_local = (version_cmd & 0x0F) == 0x00;
    if is_local {
        let mut skip = vec![0u8; addr_len];
        read_exact_from_stream(stream, &mut skip).await?;
        return Err(std::io::Error::new(std::io::ErrorKind::Other, "PROXY LOCAL"));
    }

    let mut addr_data = vec![0u8; addr_len];
    read_exact_from_stream(stream, &mut addr_data).await?;

    let (src, dst) = match family {
        0x11 => {
            if addr_data.len() < 12 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData, "PROXY v2: truncated IPv4",
                ));
            }
            let src_ip = IpAddr::V4(std::net::Ipv4Addr::new(
                addr_data[0], addr_data[1], addr_data[2], addr_data[3],
            ));
            let dst_ip = IpAddr::V4(std::net::Ipv4Addr::new(
                addr_data[4], addr_data[5], addr_data[6], addr_data[7],
            ));
            let src_port = u16::from_be_bytes([addr_data[8], addr_data[9]]);
            let dst_port = u16::from_be_bytes([addr_data[10], addr_data[11]]);
            (SocketAddr::new(src_ip, src_port), SocketAddr::new(dst_ip, dst_port))
        }
        0x21 => {
            if addr_data.len() < 36 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData, "PROXY v2: truncated IPv6",
                ));
            }
            let mut src_b = [0u8; 16];
            src_b.copy_from_slice(&addr_data[0..16]);
            let mut dst_b = [0u8; 16];
            dst_b.copy_from_slice(&addr_data[16..32]);
            let src_ip = IpAddr::V6(std::net::Ipv6Addr::from(src_b));
            let dst_ip = IpAddr::V6(std::net::Ipv6Addr::from(dst_b));
            let src_port = u16::from_be_bytes([addr_data[32], addr_data[33]]);
            let dst_port = u16::from_be_bytes([addr_data[34], addr_data[35]]);
            (SocketAddr::new(src_ip, src_port), SocketAddr::new(dst_ip, dst_port))
        }
        _ => {
            let mut skip = vec![0u8; addr_len];
            read_exact_from_stream(stream, &mut skip).await?;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("PROXY v2: unknown family 0x{family:02x}"),
            ));
        }
    };

    Ok(ProxyHeader { source: src, destination: dst, is_v2: true })
}

/// Read exactly `buf.len()` bytes from a TcpStream.
async fn read_exact_from_stream(stream: &tokio::net::TcpStream, buf: &mut [u8]) -> std::io::Result<()> {
    let mut offset = 0;
    while offset < buf.len() {
        stream.readable().await?;
        match stream.try_read(&mut buf[offset..]) {
            Ok(0) => return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof, "proxy protocol: connection closed",
            )),
            Ok(n) => offset += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Serve an axum app with PROXY protocol support.
///
/// Accepts TCP connections, detects PROXY headers via `accept_proxy_or_direct`,
/// and injects [`RealClientIp`] into request extensions before forwarding to axum.
///
/// Direct connections (no PROXY header) pass through untouched.
pub async fn serve_axum(
    listener: tokio::net::TcpListener,
    app: axum::Router,
) -> std::io::Result<()> {
    loop {
        let (stream, _peer_addr) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(?e, "PROXY listener accept failed");
                continue;
            }
        };

        let app = app.clone();
        tokio::spawn(async move {
            let (stream, real_ip) = accept_proxy_or_direct(stream).await;

            let svc = hyper::service::service_fn(
                move |mut req: hyper::Request<hyper::body::Incoming>| {
                    if let Some(ip) = real_ip {
                        req.extensions_mut().insert(RealClientIp(ip));
                    }
                    let app = app.clone();
                    async move {
                        use tower::ServiceExt;
                        app.oneshot(req).await
                    }
                },
            );

            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(stream, svc)
                .await
            {
                tracing::debug!(?e, "PROXY connection error");
            }
        });
    }
}

/// Extension key injected into requests that arrived via PROXY protocol.
///
/// Read this from `req.extensions().get::<RealClientIp>()` in handlers
/// that need the original client IP behind a reverse proxy.
#[derive(Debug, Clone, Copy)]
pub struct RealClientIp(pub std::net::IpAddr);

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn test_header_bytes(data: &[u8]) -> Vec<u8> {
        data.to_vec()
    }

    #[tokio::test]
    async fn proxy_v1_tcp4() {
        let data = test_header_bytes(b"PROXY TCP4 192.168.1.1 10.0.0.1 12345 80\r\n");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (_stream, real_ip) = accept_proxy_or_direct(stream).await;
            assert_eq!(real_ip.unwrap().to_string(), "192.168.1.1");
        });

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        // Write PROXY header
        let mut writer = stream;
        writer.write_all(&data).await.unwrap();
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn direct_connection_returns_none() {
        let data = test_header_bytes(b"GET / HTTP/1.1\r\nHost: example.com\r\n");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (_stream, real_ip) = accept_proxy_or_direct(stream).await;
            assert!(real_ip.is_none());
        });

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut writer = stream;
        writer.write_all(&data).await.unwrap();
        writer.shutdown().await.unwrap();
    }
}
