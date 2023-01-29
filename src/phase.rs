use std::net::SocketAddr;
use std::sync::Arc;

use drax::throw_explain;
use mcprotocol::handshaking::{ConnectionProtocol, HandshakingRegistry};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::client::{MCConnection, McPacketReader};
use crate::crypto::MCPrivateKey;
use crate::phase::login::LoginServer;
use crate::phase::status::StatusBuilder;

pub mod login;
pub mod play;
pub mod status;

#[derive(Debug, Clone)]
pub struct ConnectionInformation {
    pub protocol_version: i32,
    pub address: SocketAddr,
    pub virtual_host: String,
    pub virtual_port: u16,
    pub compression_threshold: Option<i32>,
}

pub async fn process_handshake<
    L: LoginServer,
    F: StatusBuilder,
    R: AsyncRead + Unpin + Send + Sync,
    W: AsyncWrite + Unpin + Send + Sync,
>(
    status_builder: Arc<F>,
    key: Arc<MCPrivateKey>,
    mut read: R,
    write: W,
    client_addr: SocketAddr,
) -> drax::prelude::Result<Option<MCConnection<R, W>>> {
    let intention_packet = read.read_packet::<HandshakingRegistry>(None).await?;

    log::info!("Intention packet: {:?}", intention_packet);
    let HandshakingRegistry::ClientIntention {
        protocol_version,
        host_name,
        port,
        intention,
    } = intention_packet;

    match intention {
        ConnectionProtocol::Play => {
            throw_explain!("Client attempted to reach play phase via handshake.")
        }
        ConnectionProtocol::Status => {
            status::accept_status_client::<F, R, W>(status_builder, read, write)
                .await
                .map(|_| None)
        }
        ConnectionProtocol::Login => {
            let connection_info = ConnectionInformation {
                protocol_version,
                address: client_addr,
                virtual_host: host_name,
                virtual_port: port,
                compression_threshold: Some(32767),
            };
            login::login_client::<L, _, _>(key, read, write, connection_info)
                .await
                .map(Some)
        }
    }
}
