use drax::{err_explain, throw_explain};
use proxy_protocol::ProxyHeader;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
use std::str::FromStr;
use tokio::io::{AsyncRead, AsyncReadExt};

const V1_HEADER_BYTES: [u8; 6] = [b'P', b'R', b'O', b'X', b'Y', b' '];
const V2_HEADER_BYTES: [u8; 12] = [
    0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A,
];

pub async fn parse_proxy_protocol(
    buf: &mut (impl AsyncRead + Unpin),
) -> drax::prelude::Result<ProxyHeader> {
    let mut first_bits = [0u8; 6];
    buf.read_exact(&mut first_bits).await?;
    log::info!("First bits: {:?}", first_bits);
    if first_bits == V1_HEADER_BYTES {
        return parse_v1_protocol(buf).await;
    }
    let mut second_bits = [0u8; 6];
    log::info!("Second bits: {:?}", first_bits);
    buf.read_exact(&mut second_bits).await?;
    let concat = [first_bits, second_bits].concat();
    if concat == V2_HEADER_BYTES {
        return parse_v2_protocol(buf).await;
    }
    throw_explain!("Invalid proxy protocol header.")
}

macro_rules! skip {
    ($buf:expr, $n:expr) => {
        tokio::io::copy(&mut $buf.take($n), &mut tokio::io::sink()).await?
    };
}

const CR: u8 = 0x0D;
const LF: u8 = 0x0A;

pub async fn read_to(
    buf: &mut (impl AsyncRead + Unpin),
    byte: u8,
) -> drax::prelude::Result<Vec<u8>> {
    let mut reader = vec![];
    loop {
        let next = buf.read_u8().await?;
        if next == byte {
            break;
        }
        reader.push(next);
    }
    Ok(reader)
}

pub async fn parse_v1_protocol(
    buf: &mut (impl AsyncRead + Unpin),
) -> drax::prelude::Result<ProxyHeader> {
    let step = buf.read_u8().await?;
    log::info!("Step: {}", step as char);
    let address_family = match step {
        b'T' => {
            log::info!("Skipping on T");
            skip!(buf, 2);
            let version = buf.read_u8().await?;
            match version {
                b'4' => 4,
                b'6' => 6,
                _ => throw_explain!("Invalid address family in proxy protocol; following T."),
            }
        }
        b'U' => {
            log::info!("Skipping for U");
            skip!(buf, 6);
            -1
        }
        _ => throw_explain!("Invalid address family in proxy protocol."),
    };

    if address_family == -1 {
        let mut cr = false;
        loop {
            let next = buf.read_u8().await?;
            if cr && next == LF {
                break;
            }
            cr = next == CR;
        }

        return Ok(ProxyHeader::Version1 {
            addresses: proxy_protocol::version1::ProxyAddresses::Unknown,
        });
    }

    skip!(buf, 1);

    let read = read_to(buf, b' ').await?;
    let source = std::str::from_utf8(&read)?;

    let read = read_to(buf, b' ').await?;
    let destination = std::str::from_utf8(&read)?;

    log::info!("Source: {}, Destination: {}", source, destination);

    let (source, destination) =
        match address_family {
            4 => (
                IpAddr::V4(
                    Ipv4Addr::from_str(source)
                        .map_err(|err| err_explain!(format!("Error parsing v4 source: {}", err)))?,
                ),
                IpAddr::V4(Ipv4Addr::from_str(destination).map_err(|err| {
                    err_explain!(format!("Error parsing v4 destination: {}", err))
                })?),
            ),
            6 => (
                IpAddr::V6(
                    Ipv6Addr::from_str(source)
                        .map_err(|err| err_explain!(format!("Error parsing v6 source: {}", err)))?,
                ),
                IpAddr::V6(Ipv6Addr::from_str(destination).map_err(|err| {
                    err_explain!(format!("Error parsing v6 destination: {}", err))
                })?),
            ),
            _ => unreachable!(),
        };

    let read = read_to(buf, b' ').await?;
    let source_port = std::str::from_utf8(&read)?;

    let read = read_to(buf, b' ').await?;
    let destination_port = std::str::from_utf8(&read)?;

    log::info!(
        "Source port: {}, Destination port: {}",
        source_port,
        destination_port
    );

    if source_port.starts_with('0') || destination_port.starts_with('0') {
        throw_explain!("Invalid ports; ports must not start with 0.");
    }

    let source_port: u16 = source_port
        .parse()
        .map_err(|err| err_explain!(format!("Error parsing source port: {}", err)))?;

    let destination_port: u16 = destination_port
        .parse()
        .map_err(|err| err_explain!(format!("Error parsing destination port: {}", err)))?;

    let mut next = [0u8; 2];
    buf.read_exact(&mut next).await?;

    log::info!("Next: {:?}", next);
    if next != [CR, LF] {
        throw_explain!("Invalid proxy protocol exit.");
    }

    let addresses = match (source, destination) {
        (IpAddr::V4(source), IpAddr::V4(destination)) => {
            proxy_protocol::version1::ProxyAddresses::Ipv4 {
                source: SocketAddrV4::new(source, source_port),
                destination: SocketAddrV4::new(destination, destination_port),
            }
        }
        (IpAddr::V6(source), IpAddr::V6(destination)) => {
            proxy_protocol::version1::ProxyAddresses::Ipv6 {
                source: SocketAddrV6::new(source, source_port, 0, 0),
                destination: SocketAddrV6::new(destination, destination_port, 0, 0),
            }
        }
        _ => unreachable!(),
    };

    Ok(ProxyHeader::Version1 { addresses })
}

pub async fn parse_v2_protocol(
    buf: &mut (impl AsyncRead + Unpin),
) -> drax::prelude::Result<ProxyHeader> {
    let version_and_command = buf.read_u8().await?;
    let version = version_and_command >> 4;
    match version {
        2 => {
            todo!()
        }
        _ => throw_explain!("Invalid proxy protocol version."),
    }
}
