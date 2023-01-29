use std::net::SocketAddr;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::Arc;

use drax::prelude::ErrorType;
use drax::{throw_explain, PinnedLivelyResult, PinnedResult};
use mcprotocol::clientbound::status::StatusResponse;
use mcprotocol::common::play::{Location, SimpleLocation};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpListener;

use crate::client::{MCConnection, ProcessedPlayer};
use crate::crypto::MCPrivateKey;
use crate::math::create_sorted_coordinates;
use crate::phase::login::LoginServer;
use crate::phase::process_handshake;
use crate::phase::status::StatusBuilder;
use crate::proxy_protocol::parse_proxy_protocol;

pub struct StatusBuilderWrapper<B> {
    count: Arc<AtomicUsize>,
    builder: B,
}

impl<B> StatusBuilder for StatusBuilderWrapper<B>
where
    B: MCServerStatusBuilder,
{
    fn build(&self) -> PinnedLivelyResult<StatusResponse> {
        self.builder.build_status(self.count.load(Ordering::SeqCst))
    }
}

pub trait MCServerStatusBuilder {
    fn build_status(&self, client_count: usize) -> PinnedLivelyResult<StatusResponse>;
}

impl<F> MCServerStatusBuilder for F
where
    F: Fn(isize) -> StatusResponse,
    F: Send + Sync,
{
    fn build_status(&self, client_count: usize) -> PinnedLivelyResult<StatusResponse> {
        Box::pin(async move { Ok((self)(client_count as isize)) })
    }
}

pub struct MCServer<F> {
    key: Arc<MCPrivateKey>,
    status_builder: Option<Arc<F>>,
    client_count: Arc<AtomicUsize>,
    entity_count_locker: Arc<AtomicI32>,
    bind: String,
    initial_location: Location,
    compression_threshold: Option<i32>,
    chunk_radius: i32,
    proxy_protocol: bool,
}

impl MCServer<()> {
    pub fn new() -> MCServer<()> {
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
            chunk_radius: 8,
            proxy_protocol: false,
        }
    }
}

impl<F> MCServer<F> {
    pub fn enable_proxy_protocol(mut self, enable: bool) -> Self {
        self.proxy_protocol = enable;
        self
    }

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

    pub fn build_status<FN>(self, builder: FN) -> MCServer<FN>
    where
        FN: StatusBuilder,
    {
        MCServer {
            key: self.key,
            status_builder: Some(Arc::new(builder)),
            client_count: self.client_count,
            bind: self.bind,
            entity_count_locker: self.entity_count_locker,
            initial_location: self.initial_location,
            compression_threshold: self.compression_threshold,
            chunk_radius: self.chunk_radius,
            proxy_protocol: self.proxy_protocol,
        }
    }

    pub fn build_mc_status<FN>(self, builder: FN) -> MCServer<StatusBuilderWrapper<FN>>
    where
        FN: MCServerStatusBuilder,
    {
        MCServer {
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
            chunk_radius: self.chunk_radius,
            proxy_protocol: self.proxy_protocol,
        }
    }

    pub fn chunk_radius(mut self, radius: i32) -> Self {
        self.chunk_radius = radius;
        self
    }
}

impl<F> MCServer<F>
where
    F: StatusBuilder + Send + Sync + 'static,
{
    pub async fn spawn<L: LoginServer, C: Clone + Send + Sync + 'static>(
        self,
        client_context: C,
        client_acceptor: fn(C, ProcessedPlayer) -> PinnedResult<()>,
    ) -> drax::prelude::Result<()> {
        let MCServer {
            key,
            status_builder,
            client_count,
            entity_count_locker,
            bind,
            initial_location,
            compression_threshold,
            chunk_radius,
            proxy_protocol,
        } = self;

        let radial_cache = create_sorted_coordinates(chunk_radius);
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
            let radial_cache = radial_cache.clone();

            tokio::spawn(async move {
                let (mut read, write) = stream.into_split();

                log::info!("New client! {:?}", addr);

                let addr = if proxy_protocol {
                    log::info!("Reading proxy protocol");
                    match parse_proxy_protocol(&mut read).await {
                        Ok(proxy_protocol::ProxyHeader::Version1 { addresses }) => {
                            match addresses {
                                proxy_protocol::version1::ProxyAddresses::Ipv4 {
                                    source, ..
                                } => SocketAddr::V4(source),
                                proxy_protocol::version1::ProxyAddresses::Ipv6 {
                                    source, ..
                                } => SocketAddr::V6(source),
                                _ => addr,
                            }
                        }
                        Ok(proxy_protocol::ProxyHeader::Version2 { addresses, .. }) => {
                            match addresses {
                                proxy_protocol::version2::ProxyAddresses::Ipv4 {
                                    source, ..
                                } => SocketAddr::V4(source),
                                proxy_protocol::version2::ProxyAddresses::Ipv6 {
                                    source, ..
                                } => SocketAddr::V6(source),
                                _ => addr,
                            }
                        }
                        Ok(_) => addr,
                        Err(err) => {
                            throw_explain!(format!("Error decoding proxy protocol {}", err))
                        }
                    }
                } else {
                    addr
                };

                log::info!("Client passed! {:?}", addr);

                match process_handshake::<L, _, _, _>(status_builder, key_clone, read, write, addr)
                    .await
                {
                    Ok(Some(client)) => {
                        let client_name = client.profile.name.clone();
                        // new player added
                        client_count.fetch_add(1, Ordering::SeqCst);
                        if let Ok(client) = ProcessedPlayer::bootstrap_client(
                            chunk_radius,
                            radial_cache,
                            compression_threshold,
                            client,
                            initial_location,
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
                Ok(())
            });
        }
    }
}

impl Default for MCServer<()> {
    fn default() -> Self {
        Self::new()
    }
}

pub type RawConnection = MCConnection<OwnedReadHalf, OwnedWriteHalf>;

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

// todo move this to a config object which we can derive from a file; or partially
#[macro_export]
macro_rules! spawn_server {
    (
        $ctx:expr, $login_server:ty,
        $(@proxy_protocol $proxy_protocol:expr,)?
        $(@bind $bind:expr,)?
        $(@status $status_builder:expr,)?
        $(@mc_status $mc_status_builder:expr,)?
        $(@compress $threshold:expr,)?
        $(@initial_location $initial_location:expr,)?
        $(@chunk_radius $chunk_radius:expr,)?
        $client_context_ident:ident, $client_ident:ident -> {$($client_acceptor_tokens:tt)*}
    ) => {
        $crate::server::MCServer::new()
            $(.enable_proxy_protocol($proxy_protocol))?
            $(.bind($bind.to_string()))?
            $(.build_status($status_builder))?
            $(.build_mc_status($mc_status_builder))?
            $(.compression_threshold($threshold))?
            $(.initial_location($initial_location))?
            $(.chunk_radius($chunk_radius))?
            .spawn::<$login_server, _>($ctx, |$client_context_ident, mut $client_ident| {
                Box::pin(async move {
                    $($client_acceptor_tokens)*
                })
            })
            .await
    };
}
