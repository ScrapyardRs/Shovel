use mcprotocol::clientbound::login::ClientboundLoginRegistry::LoginDisconnect;
use mcprotocol::clientbound::status::{StatusPlayers, StatusResponse, StatusVersion};
use mcprotocol::common::chat::Chat;
use shovel::client::McPacketWriter;
use shovel::crypto::MCPrivateKey;
use shovel::phase::process_handshake;
use shovel::phase::status::StatusBuilder;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpListener;

pub struct BasicStatusResponse;

impl StatusBuilder for BasicStatusResponse {
    fn build(&mut self) -> StatusResponse {
        let mut description = Chat::text("Sample Motd");
        description.modify_style(|style| style.color("green"));

        StatusResponse {
            description,
            players: StatusPlayers {
                max: 200,
                online: -200,
                sample: vec![],
            },
            version: StatusVersion {
                name: shovel::version_constants::CURRENT_PROTOCOL_VERSION_STRING.to_string(),
                protocol: shovel::version_constants::CURRENT_PROTOCOL_VERSION,
            },
            favicon: None,
            enforces_secure_chat: false,
        }
    }
}

async fn process_new_client(
    server_key: Arc<MCPrivateKey>,
    read: OwnedReadHalf,
    write: OwnedWriteHalf,
    addr: SocketAddr,
) -> drax::prelude::Result<()> {
    let client = process_handshake::<BasicStatusResponse, _, _>(
        &mut BasicStatusResponse,
        server_key,
        read,
        write,
        addr,
    )
    .await?;
    if let Some(mut client) = client {
        client.compress(20000).await?;
        let mut writer = client.writer.write().await;
        writer
            .writer
            .write_compressed_packet(
                20000,
                &LoginDisconnect {
                    reason: "Completion".into(),
                },
            )
            .await?;
        drop(writer);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> drax::prelude::Result<()> {
    let server_key = Arc::new(shovel::crypto::new_key().unwrap());

    let listener = TcpListener::bind("0.0.0.0:25565").await?;
    loop {
        let (stream, addr) = listener.accept().await?;
        let (read, write) = stream.into_split();
        let local = tokio::task::LocalSet::new();
        let key = server_key.clone();
        if let Err(err) = local
            .run_until(async move {
                let server_key = key;
                tokio::task::spawn_local(async move {
                    process_new_client(server_key, read, write, addr).await
                })
                .await
                .unwrap()
            })
            .await
        {
            eprintln!("Error! {}", err)
        }
    }
}
