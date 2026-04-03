use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::UdpSocket;
use tracing::info;

/// Default public STUN servers
const STUN_SERVERS: &[&str] = &[
    "stun.l.google.com:19302",
    "stun1.l.google.com:19302",
    "stun.cloudflare.com:3478",
];

/// STUN message types
const STUN_BINDING_REQUEST: u16 = 0x0001;
const STUN_BINDING_RESPONSE: u16 = 0x0101;
const STUN_MAGIC_COOKIE: u32 = 0x2112A442;
const STUN_ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const STUN_ATTR_MAPPED_ADDRESS: u16 = 0x0001;

/// Discover our public IP:port by querying STUN servers.
/// Returns the external address as seen by the STUN server.
pub async fn discover_public_addr() -> Result<SocketAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let local = socket.local_addr()?;
    info!("STUN discovery from local {}", local);

    for server in STUN_SERVERS {
        match query_stun_server(&socket, server).await {
            Ok(addr) => {
                info!("STUN: public address is {} (via {})", addr, server);
                return Ok(addr);
            }
            Err(e) => {
                info!("STUN server {} failed: {}", server, e);
                continue;
            }
        }
    }

    anyhow::bail!("All STUN servers failed")
}

/// Discover public address and return both the socket and the external address.
/// The socket can be reused for the actual data transfer (preserves NAT mapping).
pub async fn discover_with_socket() -> Result<(UdpSocket, SocketAddr)> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let local = socket.local_addr()?;
    info!("STUN discovery from local {}", local);

    for server in STUN_SERVERS {
        match query_stun_server(&socket, server).await {
            Ok(addr) => {
                info!("STUN: public address is {} (via {})", addr, server);
                return Ok((socket, addr));
            }
            Err(e) => {
                info!("STUN server {} failed: {}", server, e);
                continue;
            }
        }
    }

    anyhow::bail!("All STUN servers failed")
}

async fn query_stun_server(socket: &UdpSocket, server: &str) -> Result<SocketAddr> {
    let server_addr: SocketAddr = tokio::net::lookup_host(server)
        .await?
        .next()
        .context("failed to resolve STUN server")?;

    // Build STUN Binding Request
    let transaction_id: [u8; 12] = rand::random();
    let mut request = Vec::with_capacity(20);
    request.extend_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
    request.extend_from_slice(&0u16.to_be_bytes()); // Length = 0 (no attributes)
    request.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    request.extend_from_slice(&transaction_id);

    socket.send_to(&request, server_addr).await?;

    // Wait for response
    let mut buf = [0u8; 512];
    let (len, _) = tokio::time::timeout(Duration::from_secs(3), socket.recv_from(&mut buf))
        .await
        .context("STUN timeout")??;

    parse_stun_response(&buf[..len], &transaction_id)
}

fn parse_stun_response(data: &[u8], expected_txn: &[u8; 12]) -> Result<SocketAddr> {
    if data.len() < 20 {
        anyhow::bail!("STUN response too short");
    }

    let msg_type = u16::from_be_bytes([data[0], data[1]]);
    if msg_type != STUN_BINDING_RESPONSE {
        anyhow::bail!("Not a binding response: 0x{:04x}", msg_type);
    }

    let msg_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    let cookie = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    if cookie != STUN_MAGIC_COOKIE {
        anyhow::bail!("Bad magic cookie");
    }

    // Verify transaction ID
    if &data[8..20] != expected_txn {
        anyhow::bail!("Transaction ID mismatch");
    }

    // Parse attributes
    let mut offset = 20;
    let end = 20 + msg_len;

    while offset + 4 <= end && offset + 4 <= data.len() {
        let attr_type = u16::from_be_bytes([data[offset], data[offset + 1]]);
        let attr_len = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
        offset += 4;

        if offset + attr_len > data.len() {
            break;
        }

        let attr_data = &data[offset..offset + attr_len];

        match attr_type {
            STUN_ATTR_XOR_MAPPED_ADDRESS => {
                return parse_xor_mapped_address(attr_data);
            }
            STUN_ATTR_MAPPED_ADDRESS => {
                return parse_mapped_address(attr_data);
            }
            _ => {}
        }

        // Attributes are padded to 4-byte boundaries
        offset += (attr_len + 3) & !3;
    }

    anyhow::bail!("No mapped address in STUN response")
}

fn parse_xor_mapped_address(data: &[u8]) -> Result<SocketAddr> {
    if data.len() < 8 {
        anyhow::bail!("XOR-MAPPED-ADDRESS too short");
    }

    let family = data[1];
    let xor_port = u16::from_be_bytes([data[2], data[3]]) ^ (STUN_MAGIC_COOKIE >> 16) as u16;

    match family {
        0x01 => {
            // IPv4
            let xor_ip = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) ^ STUN_MAGIC_COOKIE;
            let ip = std::net::Ipv4Addr::from(xor_ip);
            Ok(SocketAddr::new(ip.into(), xor_port))
        }
        _ => anyhow::bail!("Unsupported address family: {}", family),
    }
}

fn parse_mapped_address(data: &[u8]) -> Result<SocketAddr> {
    if data.len() < 8 {
        anyhow::bail!("MAPPED-ADDRESS too short");
    }

    let family = data[1];
    let port = u16::from_be_bytes([data[2], data[3]]);

    match family {
        0x01 => {
            let ip = std::net::Ipv4Addr::new(data[4], data[5], data[6], data[7]);
            Ok(SocketAddr::new(ip.into(), port))
        }
        _ => anyhow::bail!("Unsupported address family: {}", family),
    }
}
