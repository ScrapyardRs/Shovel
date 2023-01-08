use drax::throw_explain;
use mcprotocol::clientbound::status::{ClientboundStatusRegistry, StatusResponse};
use mcprotocol::serverbound::status::ServerboundStatusRegistry;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::client::{McPacketReader, McPacketWriter};

pub trait StatusBuilder {
    fn build(&mut self) -> StatusResponse;
}

pub async fn accept_status_client<F: StatusBuilder, R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    status_builder: &mut F,
    mut read: R,
    mut write: W,
) -> drax::prelude::Result<()> {
    match read.read_packet::<ServerboundStatusRegistry>().await? {
        ServerboundStatusRegistry::Request => {
            write
                .write_packet(&ClientboundStatusRegistry::Response {
                    response: F::build(status_builder),
                })
                .await?;
        }
        ServerboundStatusRegistry::Ping { .. } => {
            throw_explain!("Received ping before status request")
        }
    }
    match read.read_packet::<ServerboundStatusRegistry>().await? {
        ServerboundStatusRegistry::Request => {
            throw_explain!("Received second status request; expected ping")
        }
        ServerboundStatusRegistry::Ping { payload } => {
            write
                .write_packet(&ClientboundStatusRegistry::Pong { payload })
                .await?;
        }
    }
    Ok(())
}
