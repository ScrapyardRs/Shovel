use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::Arc;

use drax::prelude::ErrorType;
use drax::{throw_explain, PinnedLivelyResult, PinnedResult};
use mcprotocol::clientbound::status::StatusResponse;
use mcprotocol::common::play::{Location, SimpleLocation};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpListener;

use crate::client::{MCClient, ShovelClient};
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
    entity_count_locker: Arc<AtomicI32>,
    bind: String,
    initial_location: Location,
    compression_threshold: Option<i32>,
}

impl MinecraftServer<()> {
    pub fn new() -> MinecraftServer<()> {
        Self {
            key: Arc::new(crate::crypto::new_key().expect("Failed to generate new server key.")),
            status_builder: None,
            client_count: Arc::new(AtomicUsize::new(0)),
            entity_count_locker: Arc::new(AtomicI32::new(0)),
            bind: "0.0.0.0:25565".to_string(),
            initial_location: Location {
                inner_loc: SimpleLocation {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                yaw: 0.0,
                pitch: 0.0,
            },
            compression_threshold: None,
        }
    }
}

impl<F> MinecraftServer<F> {
    pub fn initial_location(mut self, loc: Location) -> Self {
        self.initial_location = loc;
        self
    }

    pub fn compression_threshold(mut self, threshold: Option<i32>) -> Self {
        self.compression_threshold = threshold;
        self
    }

    pub fn bind(mut self, bind_str: String) -> Self {
        self.bind = bind_str;
        self
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
            entity_count_locker: self.entity_count_locker,
            initial_location: self.initial_location,
            compression_threshold: self.compression_threshold,
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
            entity_count_locker: self.entity_count_locker,
            initial_location: self.initial_location,
            compression_threshold: self.compression_threshold,
        }
    }
}

impl<F> MinecraftServer<F>
where
    F: StatusBuilder + Send + Sync + 'static,
{
    pub async fn spawn<C: Clone + Send + Sync + 'static>(
        self,
        client_context: C,
        client_acceptor: fn(C, ShovelClient) -> PinnedResult<()>,
    ) -> drax::prelude::Result<()> {
        let MinecraftServer {
            key,
            status_builder,
            client_count,
            entity_count_locker,
            bind,
            initial_location,
            compression_threshold,
        } = self;

        let listener = TcpListener::bind(bind).await?;

        loop {
            let (stream, addr) = listener.accept().await?;
            let client_count = client_count.clone();
            let key_clone = key.clone();
            let status_builder = status_builder
                .as_ref()
                .cloned()
                .map(Ok)
                .unwrap_or_else(|| throw_explain!("No status builder provided."))?;
            let client_context = client_context.clone();
            let entity_count_locker = entity_count_locker.clone();

            tokio::spawn(async move {
                let (read, write) = stream.into_split();
                match process_handshake(status_builder, key_clone, read, write, addr).await {
                    Ok(Some(client)) => {
                        let client_name = client.profile.name.clone();
                        // new player added
                        client_count.fetch_add(1, Ordering::SeqCst);
                        if let Ok(client) = ShovelClient::bootstrap_client(
                            compression_threshold,
                            client,
                            initial_location.clone(),
                            entity_count_locker.fetch_add(1, Ordering::SeqCst),
                            client_count,
                        )
                        .await
                        {
                            match (client_acceptor)(client_context, client).await {
                                Ok(_) => {
                                    log::info!("Client {} disconnected naturally.", client_name);
                                }
                                Err(err) if matches!(err.error_type, ErrorType::EOF) => {
                                    log::info!("Client {} disconnected with EOF.", client_name);
                                }
                                Err(err) => {
                                    log::error!("Transport error in client acceptor: {}", err);
                                }
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        if !matches!(e.error_type, ErrorType::EOF) {
                            log::error!("Error processing client: {}", e);
                        }
                    }
                }
            });
        }
    }
}

impl Default for MinecraftServer<()> {
    fn default() -> Self {
        Self::new()
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
        max: $max_players:expr,
        online: $online_players:expr,
        $(sample: $player_sample:expr,)?
        $(favicon: $favicon:expr,)?
        $(enforce: $enforce:expr,)?
    ) => {
        mcprotocol::clientbound::status::StatusResponse {
            description: $description,
            players: mcprotocol::clientbound::status::StatusPlayers {
                max: $max_players,
                online: $online_players,
                sample: $crate::__internal_status_flip!(vec![]$(, $player_sample)?),
            },
            version: mcprotocol::clientbound::status::StatusVersion {
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
        $ctx:expr,
        $(@bind $bind:expr,)?
        $(@status $status_builder:expr,)?
        $(@mc_status $mc_status_builder:expr,)?
        $(@compress $threshold:expr,)?
        $(@initial_location $initial_location:expr,)?
        $client_context_ident:ident, $client_ident:ident -> {$($client_acceptor_tokens:tt)*}
    ) => {
        $crate::server::MinecraftServer::new()
            $(.bind($bind.to_string()))?
            $(.build_status($status_builder))?
            $(.build_mc_status($mc_status_builder))?
            .spawn($ctx, |$client_context_ident, mut $client_ident| {
                Box::pin(async move {
                    $($client_acceptor_tokens)*
                })
            })
            .await
    };
}
