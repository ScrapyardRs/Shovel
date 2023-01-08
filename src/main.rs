use drax::PinnedLivelyResult;
use mcprotocol::clientbound::status::{StatusPlayers, StatusResponse, StatusVersion};
use mcprotocol::common::chat::Chat;

use shovel::server::MinecraftServerStatusBuilder;
use shovel::{spawn_server, status_builder};

pub struct BasicStatusResponse;

impl MinecraftServerStatusBuilder for BasicStatusResponse {
    fn build_status(&self, client_count: usize) -> PinnedLivelyResult<StatusResponse> {
        Box::pin(async move {
            let mut description = Chat::text("Sample Motd");
            description.modify_style(|style| style.color("green"));

            Ok(status_builder! {
                description: description,
                max: 200,
                online: client_count as isize,
            })
        })
    }
}

#[tokio::main]
async fn main() -> drax::prelude::Result<()> {
    spawn_server! {
        @mc_status BasicStatusResponse,
        client -> {
            println!("Client connected: {:?}!", client.profile);
            Ok(())
        }
    }
}
