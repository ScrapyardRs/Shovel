use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use drax::{PinnedLivelyResult, PinnedResult};
use mcprotocol::clientbound::status::StatusResponse;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpListener;
use tokio::runtime::Builder;
use tokio::task::LocalSet;

use crate::client::MCClient;
use crate::crypto::MCPrivateKey;
use crate::phase::process_handshake;
use crate::phase::status::StatusBuilder;

pub struct StatusBuilderWrapper<B> {
    count: Arc<AtomicUsize>,
    builder: B,
}

impl<B> StatusBuilder for StatusBuilderWrapper<B>
where
    B: MinecraftServerStatusBuilder,
{
    fn build(&self) -> PinnedLivelyResult<StatusResponse> {
        self.builder.build_status(self.count.load(Ordering::SeqCst))
    }
}

pub trait MinecraftServerStatusBuilder {
    fn build_status(&self, client_count: usize) -> PinnedLivelyResult<StatusResponse>;
}

pub struct MinecraftServer<F> {
    key: Arc<MCPrivateKey>,
    status_builder: Option<Arc<F>>,
    client_count: Arc<AtomicUsize>,
    bind: String,
}

impl MinecraftServer<()> {
    pub fn new() -> MinecraftServer<()> {
        Self {
            key: Arc::new(crate::crypto::new_key().expect("Failed to generate new server key.")),
            status_builder: None,
            client_count: Arc::new(AtomicUsize::new(0)),
            bind: "0.0.0.0:25565".to_string(),
        }
    }
}

impl<F> MinecraftServer<F> {
    pub fn bind(self, bind_str: String) -> MinecraftServer<F> {
        MinecraftServer {
            key: self.key,
            bind: bind_str,
            client_count: self.client_count,
            status_builder: self.status_builder,
        }
    }

    pub fn build_status<FN>(self, builder: FN) -> MinecraftServer<FN>
    where
        FN: StatusBuilder,
    {
        MinecraftServer {
            key: self.key,
            status_builder: Some(Arc::new(builder)),
            client_count: self.client_count,
            bind: self.bind,
        }
    }

    pub fn build_mc_status<FN>(self, builder: FN) -> MinecraftServer<StatusBuilderWrapper<FN>>
    where
        FN: MinecraftServerStatusBuilder,
    {
        MinecraftServer {
            key: self.key,
            status_builder: Some(Arc::new(StatusBuilderWrapper {
                count: self.client_count.clone(),
                builder,
            })),
            client_count: self.client_count,
            bind: self.bind,
        }
    }
}

impl<F> MinecraftServer<F>
where
    F: StatusBuilder + Send + Sync + 'static,
{
    pub async fn spawn(
        self,
        client_acceptor: fn(ServerPlayer) -> PinnedResult<()>,
    ) -> drax::prelude::Result<()> {
        let MinecraftServer {
            key,
            status_builder,
            client_count,
            bind,
        } = self;

        let listener = TcpListener::bind(bind).await?;

        loop {
            let (stream, addr) = listener.accept().await?;
            let client_count = client_count.clone();
            let key_clone = key.clone();
            let status_builder = status_builder.as_ref().cloned().unwrap();

            let rt = Builder::new_current_thread().enable_all().build().unwrap();

            std::thread::spawn(move || {
                let local = LocalSet::new();

                local.spawn_local(async move {
                    let (read, write) = stream.into_split();
                    match tokio::task::spawn_local(process_handshake(
                        status_builder,
                        key_clone,
                        read,
                        write,
                        addr,
                    ))
                    .await
                    .unwrap()
                    {
                        Ok(Some(client)) => {
                            // new player added
                            client_count.fetch_add(1, Ordering::SeqCst);
                            if let Err(err) = tokio::task::spawn_local((client_acceptor)(client))
                                .await
                                .unwrap()
                            {
                                log::error!("Error processing client: {}", err)
                            }
                            // remove new player
                            client_count.fetch_sub(1, Ordering::SeqCst);
                        }
                        Ok(None) => {}
                        Err(e) => {
                            log::error!("Error processing client: {}", e);
                        }
                    }
                    ();
                });

                rt.block_on(local);
            });
        }
    }
}

pub type ServerPlayer = MCClient<OwnedReadHalf, OwnedWriteHalf>;

#[macro_export]
macro_rules! __internal_status_flip {
    ($either:expr) => {
        $either
    };
    ($__:expr, $or:expr) => {
        $or
    };
}

#[macro_export]
macro_rules! status_builder {
    (
        description: $description:expr,
        max: $max_players:literal,
        online: $online_players:expr,
        $(sample: $player_sample:expr,)?
        $(favicon: $favicon:expr,)?
        $(enforce: $enforce:expr,)?
    ) => {
        mcprotocol::clientbound::status::StatusResponse {
            description: $description,
            players: StatusPlayers {
                max: $max_players,
                online: $online_players,
                sample: $crate::__internal_status_flip!(vec![]$(, $player_sample)?),
            },
            version: StatusVersion {
                name: $crate::version_constants::CURRENT_PROTOCOL_VERSION_STRING.to_string(),
                protocol: $crate::version_constants::CURRENT_PROTOCOL_VERSION,
            },
            favicon: $crate::__internal_status_flip!(None$(, $favicon)?),
            enforces_secure_chat: $crate::__internal_status_flip!(false$(, $enforce)?),
        }
    };
}

#[macro_export]
macro_rules! spawn_server {
    (
        $(@bind $bind:expr,)?
        $(@status $status_builder:expr,)?
        $(@mc_status $mc_status_builder:expr,)?
        $client_ident:ident -> {$($client_acceptor_tokens:tt)*}
    ) => {
        $crate::server::MinecraftServer::new()
            $(.bind($bind.to_string()))?
            $(.build_status($status_builder))?
            $(.build_mc_status($mc_status_builder))?
            .spawn(|$client_ident| {
                Box::pin(async move {
                    $($client_acceptor_tokens)*
                })
            })
            .await
    };
}
