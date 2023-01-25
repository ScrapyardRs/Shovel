use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use drax::{PinnedLivelyResult, throw_explain};
use drax::prelude::{DraxReadExt, DraxWriteExt, ErrorType, PacketComponent, Size};
use drax::transport::buffer::var_num::size_var_int;
use drax::transport::encryption::{Cipher, NewCipher};
use mcprotocol::clientbound::login::ClientboundLoginRegistry::{
    LoginCompression, LoginGameProfile,
};
use mcprotocol::clientbound::play::{ClientboundPlayRegistry, RelativeArgument};
use mcprotocol::clientbound::play::ClientboundPlayRegistry::{
    ClientLogin, CustomPayload, KeepAlive, PlayerPosition, SetDefaultSpawnPosition,
};
use mcprotocol::common::GameProfile;
use mcprotocol::common::play::{BlockPos, Location, SimpleLocation};
use mcprotocol::serverbound::play::ServerboundPlayRegistry;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::RwLock;

use crate::phase::ConnectionInformation;
use crate::phase::play::{ClientLoginProperties, ConnectedPlayer};
use crate::server::RawConnection;

pub struct WrappedPacketWriter<W> {
    pub writer: W,
    pub cipher: Option<Cipher>,
}

impl<W> WrappedPacketWriter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            cipher: None,
        }
    }

    pub fn attach_cipher(&mut self, cipher: Cipher) {
        self.cipher = Some(cipher);
    }

    pub fn into_inner(self) -> W {
        self.writer
    }

    pub fn write_and_cipher(&mut self) -> (&mut W, Option<&mut Cipher>) {
        (&mut self.writer, self.cipher.as_mut())
    }

    pub async fn write_play_packet(
        &mut self,
        packet: &ClientboundPlayRegistry,
    ) -> drax::transport::Result<()>
    where
        W: AsyncWrite + Unpin + Send + Sync,
    {
        let (writer, cipher) = self.write_and_cipher();
        writer.write_packet(cipher, packet).await
    }
}

pub trait McPacketReader {
    fn read_packet<'a, P: PacketComponent<()>>(
        &'a mut self,
        cipher: Option<&'a mut Cipher>,
    ) -> PinnedLivelyResult<'a, P::ComponentType>;

    fn read_compressed_packet<'a, P: PacketComponent<()>>(
        &'a mut self,
        cipher: Option<&'a mut Cipher>,
    ) -> PinnedLivelyResult<'a, P::ComponentType>;
}

impl<A: AsyncRead + Unpin + Send + Sync + Sized> McPacketReader for A {
    fn read_packet<'a, P: PacketComponent<()>>(
        &'a mut self,
        cipher: Option<&'a mut Cipher>,
    ) -> PinnedLivelyResult<'a, P::ComponentType> {
        Box::pin(async move {
            let (cipher, packet_size) = if let Some(cipher) = cipher {
                let mut decrypt = <Self as DraxReadExt>::decrypt(self, cipher);
                let size = decrypt.read_var_int().await?;
                drop(decrypt);
                (Some(cipher), size)
            } else {
                (None, self.read_var_int().await?)
            };
            let mut next_bytes = self.take(packet_size as u64);
            let mut buffer = Cursor::new(Vec::with_capacity(packet_size as usize));
            if tokio::io::copy(&mut next_bytes, &mut buffer).await? != packet_size as u64
                || next_bytes.limit() > 0
            {
                throw_explain!("Packet size mismatch");
            }
            let mut buffer = Cursor::new(if let Some(cipher) = cipher {
                let mut buffer = buffer.into_inner();
                drax::transport::encryption::AsyncStreamCipher::decrypt(cipher, &mut buffer);
                buffer
            } else {
                buffer.into_inner()
            });
            let packet = P::decode(&mut (), &mut buffer).await;
            packet
        })
    }

    fn read_compressed_packet<'a, P: PacketComponent<()>>(
        &'a mut self,
        _cipher: Option<&'a mut Cipher>,
    ) -> PinnedLivelyResult<'a, P::ComponentType> {
        Box::pin(async move { todo!("read_compressed_packet") })
    }
}

pub fn prepare_compressed_packet<P: PacketComponent<(), ComponentType = P> + Send + Sync>(
    _threshold: i32,
    _packet: &P,
) -> PinnedLivelyResult<(i32, Option<Vec<u8>>)> {
    Box::pin(async move { todo!("prepare_compressed_packet") })
}

pub trait McPacketWriter {
    fn write_compressed_packet<'a, P: PacketComponent<(), ComponentType = P> + Send + Sync>(
        &'a mut self,
        _cipher: Option<&'a mut Cipher>,
        _threshold: i32,
        _packet: &'a P,
    ) -> PinnedLivelyResult<'a, ()>
    where
        Self: AsyncWrite + Unpin + Send + Sync,
    {
        Box::pin(async move { todo!("write_compressed_packet") })
    }

    fn write_packet<'a, P: PacketComponent<(), ComponentType = P> + Send + Sync>(
        &'a mut self,
        cipher: Option<&'a mut Cipher>,
        packet: &'a P,
    ) -> PinnedLivelyResult<'a, ()>;
}

impl<A: AsyncWrite + Unpin + Send + Sync> McPacketWriter for A {
    fn write_packet<'a, P: PacketComponent<(), ComponentType = P> + Send + Sync>(
        &'a mut self,
        cipher: Option<&'a mut Cipher>,
        packet: &'a P,
    ) -> PinnedLivelyResult<'a, ()> {
        Box::pin(async move {
            let size = match P::size(packet, &mut ())? {
                Size::Dynamic(x) | Size::Constant(x) => x as i32,
            };
            let len = size_var_int(size) + size as usize;
            let mut buffer = Cursor::new(Vec::with_capacity(len));
            buffer.write_var_int(size).await?;
            P::encode(&packet, &mut (), &mut buffer).await?;
            let mut buffer = buffer.into_inner();
            if let Some(cipher) = cipher {
                drax::transport::encryption::AsyncStreamCipher::encrypt(cipher, &mut buffer);
            }
            self.write_all(&buffer).await?;
            Ok(())
        })
    }
}

pub struct MCConnection<R, W> {
    pub cipher: Option<Cipher>,
    pub reader: R,
    pub writer: WrappedPacketWriter<W>,
    compression_threshold: Option<i32>,
    pub connection_information: ConnectionInformation,
    pub profile: GameProfile,
    logged_in: bool,
}

impl<R, W> MCConnection<R, W> {
    pub fn new(
        cipher_key: Option<&[u8]>,
        reader: R,
        writer: W,
        connection_information: ConnectionInformation,
        profile: GameProfile,
    ) -> Self {
        Self {
            cipher: cipher_key.map(|key| NewCipher::new_from_slices(key, key).unwrap()),
            reader,
            writer: WrappedPacketWriter {
                writer,
                cipher: cipher_key.map(|key| NewCipher::new_from_slices(key, key).unwrap()),
            },
            compression_threshold: None,
            connection_information,
            profile,
            logged_in: false,
        }
    }
}

impl<R, W> MCConnection<R, W>
where
    R: AsyncRead + Unpin + Send + Sync,
{
    pub async fn read_packet<P: PacketComponent<()>>(
        &mut self,
    ) -> drax::prelude::Result<P::ComponentType> {
        match self.compression_threshold {
            None => self.reader.read_packet::<P>(self.cipher.as_mut()).await,
            Some(_) => todo!(),
        }
    }
}

impl<R, W> MCConnection<R, W>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    pub async fn complete_login(&mut self) -> drax::prelude::Result<()> {
        if self.logged_in {
            throw_explain!("Client already logged in at this point.");
        }
        let (writer, cipher) = self.writer.write_and_cipher();
        writer
            .write_packet(
                cipher,
                &LoginGameProfile {
                    game_profile: self.profile.clone(),
                },
            )
            .await?;
        self.logged_in = true;
        Ok(())
    }

    pub async fn compress_and_complete_login(
        &mut self,
        threshold: i32,
    ) -> drax::transport::Result<()> {
        if self.logged_in {
            throw_explain!("Client already logged in at this point.");
        }
        self.compression_threshold = Some(threshold);
        let (writer, cipher) = self.writer.write_and_cipher();
        writer
            .write_packet(cipher, &LoginCompression { threshold })
            .await?;
        let (writer, cipher) = self.writer.write_and_cipher();
        writer
            .write_compressed_packet(
                cipher,
                threshold,
                &LoginGameProfile {
                    game_profile: self.profile.clone(),
                },
            )
            .await?;
        self.logged_in = true;
        Ok(())
    }
}

pub struct PendingPosition {
    pub(crate) location: Location,
    pub(crate) pending_teleport: Option<(Location, i32)>,
    pub(crate) on_ground: bool,
    pub(crate) is_loaded: bool,
}

pub struct ProcessedPlayer {
    pub server_player: RawConnection,
    pub current_player_position: Location,
    pub pending_position: Arc<RwLock<PendingPosition>>,
    pub entity_id: i32,
    pub client_count_ref: Arc<AtomicUsize>,
}

impl ProcessedPlayer {
    pub async fn bootstrap_client(
        compression: Option<i32>,
        mut player: RawConnection,
        initial_position: Location,
        player_id: i32,
        client_count: Arc<AtomicUsize>,
    ) -> drax::prelude::Result<Self> {
        if let Some(compression) = compression {
            player.compress_and_complete_login(compression).await?;
        } else {
            player.complete_login().await?;
        }

        Ok(Self {
            server_player: player,
            current_player_position: initial_position.clone(),
            pending_position: Arc::new(RwLock::new(PendingPosition {
                location: initial_position.clone(),
                pending_teleport: Some((initial_position.clone(), 0)),
                on_ground: false,
                is_loaded: false,
            })),
            entity_id: player_id,
            client_count_ref: client_count,
        })
    }

    pub async fn send_client_login<S: Into<String>>(
        &mut self,
        brand: S,
        arguments: RelativeArgument,
        properties: ClientLoginProperties,
    ) -> drax::prelude::Result<()> {
        let ClientLoginProperties {
            hardcore,
            game_type,
            previous_game_type,
            seed,
            max_players,
            chunk_radius,
            simulation_distance,
            reduced_debug_info,
            show_death_screen,
            is_debug,
            is_flat,
            last_death_location,
        } = properties;
        self.server_player
            .writer
            .write_play_packet(&ClientLogin {
                player_id: self.entity_id,
                hardcore,
                game_type,
                previous_game_type,
                levels: vec![
                    "minecraft:overworld".to_string(),
                    "minecraft:the_end".to_string(),
                    "minecraft:the_nether".to_string(),
                ],
                codec: crate::phase::play::get_current_dimension_snapshot().await?,
                dimension_type: "minecraft:overworld".to_string(),
                dimension: "minecraft:overworld".to_string(),
                seed,
                max_players,
                chunk_radius,
                simulation_distance,
                reduced_debug_info,
                show_death_screen,
                is_debug,
                is_flat,
                last_death_location,
            })
            .await?;
        self.server_player
            .writer
            .write_play_packet(&KeepAlive { id: 0 })
            .await?;

        let mut brand_data = Cursor::new(Vec::new());
        String::encode(&brand.into(), &mut (), &mut brand_data).await?;
        self.server_player
            .writer
            .write_play_packet(&CustomPayload {
                identifier: format!("minecraft:brand"),
                data: brand_data.into_inner(),
            })
            .await?;

        let inner = self.current_player_position.inner_loc;

        self.server_player
            .writer
            .write_play_packet(&SetDefaultSpawnPosition {
                pos: BlockPos {
                    x: inner.x as i32,
                    y: inner.y as i32,
                    z: inner.z as i32,
                },
                angle: 0.0,
            })
            .await?;

        let position_packet = PlayerPosition {
            location: self.current_player_position,
            relative_arguments: arguments,
            id: self.entity_id,
            dismount: false,
        };

        self.server_player
            .writer
            .write_play_packet(&position_packet)
            .await
    }

    pub async fn keep_alive<F: FnOnce(ConnectedPlayer)>(mut self, func: F) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let profile_clone = self.server_player.profile.clone();
        let cloned_pending_position = self.pending_position.clone();
        let current_player_position = self.current_player_position;
        let entity_id = self.entity_id;
        let client_count = self.client_count_ref;
        let connection_information = self.server_player.connection_information.clone();

        // let connected_player = ConnectedPlayer { // todo
        //     writer: passed_cloned_writer,
        //     profile: profile_clone,
        //     packets: PacketLocker {
        //         packet_listener: rx,
        //         active: true,
        //         connection_information,
        //     },
        //     position: current_player_position,
        //     pending_position: cloned_pending_position,
        //     entity_id,
        //     tracking: TrackingDetails::default(),
        //     teleport_id_incr: AtomicI32::new(1),
        // };
        //
        // (func)(connected_player);

        let pending_position = self.pending_position;
        let mut seq = 0;
        loop {
            let packet = match self
                .server_player
                .read_packet::<ServerboundPlayRegistry>()
                .await
            {
                Ok(packet) => packet,
                Err(err) => {
                    if !matches!(err.error_type, ErrorType::EOF) {
                        log::error!("Error during client read: {}", err);
                    }
                    break;
                }
            };
            match &packet {
                ServerboundPlayRegistry::KeepAlive { keep_alive_id } => {
                    let cloned_writer = cloned_writer.clone();
                    if *keep_alive_id == seq {
                        seq += 1;
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            if let Err(err) =
                                cloned_writer.write_packet(&KeepAlive { id: seq }).await
                            {
                                log::error!("Error sending keep alive: {}", err);
                            };
                        });
                        continue;
                    } else {
                        break;
                    }
                }
                ServerboundPlayRegistry::AcceptTeleportation { teleportation_id } => {
                    let mut pending_position = pending_position.write().await;
                    if let Some(pending_teleport) = pending_position.pending_teleport.take() {
                        if pending_teleport.1 != *teleportation_id {
                            pending_position.pending_teleport = Some(pending_teleport);
                        } else {
                            pending_position.location = pending_teleport.0;
                        }
                    }
                }
                ServerboundPlayRegistry::MovePlayerPos { x, y, z, on_ground } => {
                    let mut lock = pending_position.write().await;
                    if lock.pending_teleport.is_some() {
                        continue;
                    }
                    lock.location.inner_loc = SimpleLocation {
                        x: *x,
                        y: *y,
                        z: *z,
                    };
                    lock.on_ground = *on_ground;
                    if !lock.is_loaded {
                        lock.is_loaded = true;
                    }
                }
                ServerboundPlayRegistry::MovePlayerPosRot {
                    x,
                    y,
                    z,
                    x_rot,
                    y_rot,
                    on_ground,
                } => {
                    let mut lock = pending_position.write().await;
                    if lock.pending_teleport.is_some() {
                        continue;
                    }
                    lock.location = Location {
                        inner_loc: SimpleLocation {
                            x: *x,
                            y: *y,
                            z: *z,
                        },
                        yaw: *y_rot,
                        pitch: *x_rot,
                    };
                    lock.on_ground = *on_ground;
                    if !lock.is_loaded {
                        lock.is_loaded = true;
                    }
                }
                ServerboundPlayRegistry::MovePlayerRot {
                    x_rot,
                    y_rot,
                    on_ground,
                } => {
                    let mut lock = pending_position.write().await;
                    if lock.pending_teleport.is_some() {
                        continue;
                    }
                    lock.location.yaw = *y_rot;
                    lock.location.pitch = *x_rot;
                    lock.on_ground = *on_ground;
                    if !lock.is_loaded {
                        lock.is_loaded = true;
                    }
                }
                ServerboundPlayRegistry::MovePlayerStatusOnly { status } => {
                    let mut lock = pending_position.write().await;
                    if lock.pending_teleport.is_some() {
                        continue;
                    }
                    lock.on_ground = *status != 0;
                    if !lock.is_loaded {
                        lock.is_loaded = true;
                    }
                }
                _ => {
                    if let Err(_) = tx.send(packet) {
                        break;
                    };
                }
            }
        }
        client_count.fetch_sub(1, Ordering::SeqCst);
    }
}
