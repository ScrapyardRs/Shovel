use std::io::Cursor;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use drax::prelude::{DraxReadExt, DraxWriteExt, ErrorType, PacketComponent, Size};
use drax::transport::buffer::var_num::size_var_int;
use drax::transport::encryption::{Cipher, NewCipher};
use drax::{throw_explain, PinnedLivelyResult};
use mcprotocol::clientbound::login::ClientboundLoginRegistry::{
    LoginCompression, LoginGameProfile,
};
use mcprotocol::clientbound::play::ClientboundPlayRegistry;
use mcprotocol::clientbound::play::ClientboundPlayRegistry::{
    ClientLogin, CustomPayload, KeepAlive, SetDefaultSpawnPosition,
};
use mcprotocol::common::play::{BlockPos, Location, SimpleLocation};
use mcprotocol::common::GameProfile;
use mcprotocol::serverbound::play::ServerboundPlayRegistry;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::phase::play::{
    ChunkPositionLoader, ClientLoginProperties, ConnectedPlayer, PacketLocker,
};
use crate::phase::ConnectionInformation;
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

            P::decode(&mut (), &mut buffer).await
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
            P::encode(packet, &mut (), &mut buffer).await?;
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
            current_player_position: initial_position,
            pending_position: Arc::new(RwLock::new(PendingPosition {
                location: initial_position,
                pending_teleport: Some((initial_position, 0)),
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
                identifier: "minecraft:brand".to_string(),
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
            .await
    }

    pub async fn keep_alive(self) -> (ConnectedPlayer, JoinHandle<()>, JoinHandle<()>) {
        // read thread
        let client_count = self.client_count_ref;
        let (packet_writer_tx, mut packet_writer_rx) = tokio::sync::mpsc::unbounded_channel();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let RawConnection {
            mut cipher,
            mut reader,
            mut writer,
            compression_threshold,
            connection_information,
            profile,
            ..
        } = self.server_player;

        let connected_player = ConnectedPlayer {
            packets: PacketLocker {
                send: packet_writer_tx.clone(),
                packet_listener: rx,
                active: true,
                connection_information,
            },
            entity_id: self.entity_id,
            profile,
            is_position_loaded: false,
            position: self.current_player_position,
            on_ground: false,
            pending_position: self.pending_position.clone(),
            teleport_id_incr: AtomicI32::new(0),
            chunk_loader: ChunkPositionLoader {
                known_chunks: Default::default(),
            },
            player_inventory: Default::default(),
        };

        let read_join_handle = tokio::spawn(async move {
            let pending_position = self.pending_position;
            let mut seq = 0;
            loop {
                let pw = packet_writer_tx.clone();
                match match match compression_threshold {
                    Some(_) => {
                        reader
                            .read_compressed_packet::<ServerboundPlayRegistry>(cipher.as_mut())
                            .await
                    }
                    None => {
                        reader
                            .read_packet::<ServerboundPlayRegistry>(cipher.as_mut())
                            .await
                    }
                } {
                    Ok(packet) => packet,
                    Err(err) => {
                        if !matches!(err.error_type, ErrorType::EOF) {
                            log::error!("Error during client read: {}", err);
                        }
                        break;
                    }
                } {
                    ServerboundPlayRegistry::KeepAlive { keep_alive_id } => {
                        if keep_alive_id == seq {
                            seq += 1;
                            tokio::spawn(async move {
                                tokio::time::sleep(Duration::from_secs(1)).await;
                                if let Err(err) = pw.send(Arc::new(KeepAlive { id: seq })) {
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
                            if pending_teleport.1 != teleportation_id {
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
                            x: x.clamp(-3.0e7, 3.0e7),
                            y: y.clamp(-2.0e7, 2.0e7),
                            z: z.clamp(-3.0e7, 3.0e7),
                        };
                        lock.on_ground = on_ground;
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
                                x: x.clamp(-3.0e7, 3.0e7),
                                y: y.clamp(-2.0e7, 2.0e7),
                                z: z.clamp(-3.0e7, 3.0e7),
                            },
                            yaw: crate::math::wrap_degrees(y_rot),
                            pitch: crate::math::wrap_degrees(x_rot),
                        };
                        lock.on_ground = on_ground;
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
                        lock.location.yaw = crate::math::wrap_degrees(y_rot);
                        lock.location.pitch = crate::math::wrap_degrees(x_rot);
                        lock.on_ground = on_ground;
                        if !lock.is_loaded {
                            lock.is_loaded = true;
                        }
                    }
                    ServerboundPlayRegistry::MovePlayerStatusOnly { status } => {
                        let mut lock = pending_position.write().await;
                        if lock.pending_teleport.is_some() {
                            continue;
                        }
                        lock.on_ground = status != 0;
                        if !lock.is_loaded {
                            lock.is_loaded = true;
                        }
                    }
                    packet => {
                        if tx.send(packet).is_err() {
                            break;
                        };
                    }
                }
            }
            client_count.fetch_sub(1, Ordering::SeqCst);
        });

        // write thread
        let write_join_handle = tokio::spawn(async move {
            while let Some(packet) = packet_writer_rx.recv().await {
                if writer.write_play_packet(&packet).await.is_err() {
                    break;
                };
            }
        });

        (connected_player, read_join_handle, write_join_handle)
    }
}

pub enum ConditionalPacket<T> {
    Unconditional(ClientboundPlayRegistry),
    Conditional(ClientboundPlayRegistry, Box<dyn for<'a> Fn(&'a T) -> bool>),
}

impl<T> ConditionalPacket<T> {
    pub fn send_to_clients<'a, I: Iterator<Item = &'a T>>(
        self,
        clients: I,
        sender_fn: fn(&'a T, Arc<ClientboundPlayRegistry>),
    ) {
        match self {
            ConditionalPacket::Unconditional(packet) => {
                let packet = Arc::new(packet);
                for client in clients {
                    sender_fn(client, packet.clone());
                }
            }
            ConditionalPacket::Conditional(packet, condition) => {
                let packet = Arc::new(packet);
                for client in clients {
                    if condition(client) {
                        sender_fn(client, packet.clone());
                    }
                }
            }
        }
    }
}
