use std::sync::Arc;

use drax::{throw_explain, PinnedLivelyResult};
use mcprotocol::clientbound::status::{ClientboundStatusRegistry, StatusResponse};
use mcprotocol::serverbound::status::ServerboundStatusRegistry;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::client::{McPacketReader, McPacketWriter};

pub trait StatusBuilder {
    fn build(&self) -> PinnedLivelyResult<StatusResponse>;
}

pub async fn accept_status_client<F: StatusBuilder, R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    status_builder: Arc<F>,
    mut read: R,
    mut write: W,
) -> drax::prelude::Result<()> {
    match read.read_packet::<ServerboundStatusRegistry>().await? {
        ServerboundStatusRegistry::Request => {
            log::trace!("Client requested status.");
            write
                .write_packet(&ClientboundStatusRegistry::Response {
                    response: F::build(&status_builder).await?,
                })
                .await?;
        }
        ServerboundStatusRegistry::Ping { .. } => {
            log::trace!("Client attempted to ping server invalidly.");
            throw_explain!("Received ping before status request")
        }
    }
    match read.read_packet::<ServerboundStatusRegistry>().await? {
        ServerboundStatusRegistry::Request => {
            log::trace!("Client requested status invalidly");
            throw_explain!("Received second status request; expected ping")
        }
        ServerboundStatusRegistry::Ping { payload } => {
            log::trace!("Received ping");
            write
                .write_packet(&ClientboundStatusRegistry::Pong { payload })
                .await?;
        }
    }
    Ok(())
}
