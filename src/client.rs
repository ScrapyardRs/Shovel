use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use async_compression::tokio::write::{ZlibDecoder, ZlibEncoder};
use drax::prelude::{DraxReadExt, DraxWriteExt, ErrorType, PacketComponent, Size};
use drax::transport::buffer::var_num::size_var_int;
use drax::transport::encryption::{DecryptRead, EncryptedWriter};
use drax::{throw_explain, PinnedLivelyResult};
use mcprotocol::clientbound::login::ClientboundLoginRegistry::{
    LoginCompression, LoginGameProfile,
};
use mcprotocol::clientbound::play::ClientboundPlayRegistry::{
    ClientLogin, CustomPayload, Disconnect, KeepAlive, PlayerPosition, SetDefaultSpawnPosition,
};
use mcprotocol::clientbound::play::RelativeArgument;
use mcprotocol::common::chat::Chat;
use mcprotocol::common::play::{BlockPos, Location, SimpleLocation};
use mcprotocol::common::GameProfile;
use mcprotocol::serverbound::play::ServerboundPlayRegistry;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::RwLock;

use crate::phase::play::ClientLoginProperties;
use crate::phase::ConnectionInformation;
use crate::server::ServerPlayer;

pub struct WrappedPacketWriter<W> {
    pub writer: W,
}

impl<W> WrappedPacketWriter<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    pub fn lock(self) -> PacketWriter<W> {
        Arc::new(RwLock::new(self))
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

pub type PacketWriter<W> = Arc<RwLock<WrappedPacketWriter<W>>>;

pub trait McPacketReader {
    fn read_packet<P: PacketComponent<()>>(&mut self) -> PinnedLivelyResult<P::ComponentType>;

    fn read_compressed_packet<P: PacketComponent<()>>(
        &mut self,
    ) -> PinnedLivelyResult<P::ComponentType>;
}

impl<A: AsyncRead + Unpin + Send + Sync> McPacketReader for A {
    fn read_packet<P: PacketComponent<()>>(&mut self) -> PinnedLivelyResult<P::ComponentType> {
        Box::pin(async move {
            let packet_size = self.read_var_int().await?;
            let mut next_bytes = self.take(packet_size as u64);
            let packet = P::decode(&mut (), &mut next_bytes).await;
            if next_bytes.limit() > 0 {
                throw_explain!("Packet was not fully read")
            }
            packet
        })
    }

    fn read_compressed_packet<P: PacketComponent<()>>(
        &mut self,
    ) -> PinnedLivelyResult<P::ComponentType> {
        Box::pin(async move {
            let compressed_packet_size = self.read_var_int().await?;
            let uncompressed_packet_size = self.read_var_int().await?;
            let mut next_bytes = self.take(compressed_packet_size as u64 - 1);
            if uncompressed_packet_size == 0 {
                let packet = next_bytes.decode_component::<(), P>(&mut ()).await;
                if next_bytes.limit() > 0 {
                    throw_explain!("Packet was not fully read")
                }
                packet
            } else {
                let mut decoder = ZlibDecoder::new(Cursor::new(Vec::with_capacity(
                    uncompressed_packet_size as usize,
                )));
                tokio::io::copy(&mut next_bytes, &mut decoder).await?;
                if next_bytes.limit() > 0 {
                    throw_explain!("Packet was not fully read")
                }
                let mut decompressed = Cursor::new(decoder.into_inner().into_inner());
                let packet = decompressed.decode_component::<(), P>(&mut ()).await;
                if decompressed.position() != decompressed.get_ref().len() as u64 {
                    throw_explain!("Decompressed packet is not fully read")
                } else {
                    packet
                }
            }
        })
    }
}

pub fn prepare_compressed_packet<P: PacketComponent<(), ComponentType = P> + Send + Sync>(
    threshold: i32,
    packet: &P,
) -> PinnedLivelyResult<(i32, Option<Vec<u8>>)> {
    Box::pin(async move {
        let size = match P::size(packet, &mut ())? {
            Size::Dynamic(x) | Size::Constant(x) => x as i32,
        };
        if size >= threshold {
            log::debug!("Preparing compressed packet.. met threshold to compress.");
            let mut zlib_compressor = ZlibEncoder::new(Cursor::new(vec![]));
            zlib_compressor
                .encode_component::<(), P>(&mut (), packet)
                .await?;
            zlib_compressor.flush().await?;
            let prepared = zlib_compressor.into_inner().into_inner();
            log::debug!(
                "Preparing compressed packet.. met threshold to compress.\n\
                Success! Compressed packet size: {} -> {}",
                size,
                prepared.len()
            );
            Ok((size, Some(prepared)))
        } else {
            log::debug!("Not compressing packet of size {}", size);
            Ok((size, None))
        }
    })
}

pub trait McPacketWriter {
    fn write_compressed_packet<'a, P: PacketComponent<(), ComponentType = P> + Send + Sync>(
        &'a mut self,
        threshold: i32,
        packet: &'a P,
    ) -> PinnedLivelyResult<'a, ()>
    where
        Self: AsyncWrite + Unpin + Send + Sync,
    {
        Box::pin(async move {
            let (packet_size, compressed) = prepare_compressed_packet(threshold, packet).await?;
            if let Some(compressed) = compressed {
                let size_v_int_size = size_var_int(packet_size as i32);
                let len = compressed.len()
                    + size_var_int(packet_size as i32)
                    + size_var_int((compressed.len() + size_v_int_size) as i32);
                let mut buffer = Cursor::new(Vec::with_capacity(len));
                buffer
                    .write_var_int(compressed.len() as i32 + size_v_int_size as i32)
                    .await?;
                buffer.write_var_int(packet_size).await?;
                buffer.write_all(&compressed).await?;
                let buffer = buffer.into_inner();
                self.write_all(&buffer).await?;
            } else {
                let len = packet_size as usize + 1 + size_var_int(packet_size + 1);
                let mut buffer = Cursor::new(Vec::with_capacity(len));
                buffer.write_var_int(packet_size + 1).await?;
                buffer.write_var_int(0).await?;
                buffer.encode_component::<(), P>(&mut (), packet).await?;
                let buffer = buffer.into_inner();
                self.write_all(&buffer).await?;
            }
            Ok(())
        })
    }

    fn write_packet<'a, P: PacketComponent<(), ComponentType = P> + Send + Sync>(
        &'a mut self,
        packet: &'a P,
    ) -> PinnedLivelyResult<'a, ()>;

    fn write_with_ref<'a, P: PacketComponent<(), ComponentType = P> + Send + Sync>(
        &'a self,
        _packet: &'a P,
    ) -> PinnedLivelyResult<'a, ()> {
        unimplemented!("write_with_ref not implemented for this type")
    }

    fn write_compressed_with_ref<'a, P: PacketComponent<(), ComponentType = P> + Send + Sync>(
        &'a self,
        _threshold: i32,
        _packet: &'a P,
    ) -> PinnedLivelyResult<'a, ()> {
        unimplemented!("write_compressed_with_ref not implemented for this type")
    }
}

impl<A: AsyncWrite + Unpin + Send + Sync> McPacketWriter for A {
    fn write_packet<'a, P: PacketComponent<(), ComponentType = P> + Send + Sync>(
        &'a mut self,
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
            let buffer = buffer.into_inner();
            self.write_all(&buffer).await?;
            self.flush().await?;
            Ok(())
        })
    }
}

pub struct MCClient<R, W> {
    pub reader: DecryptRead<R>,
    pub writer: PacketWriter<EncryptedWriter<W>>,
    compression_threshold: Option<i32>,
    pub connection_information: ConnectionInformation,
    pub username: String,
    pub profile: GameProfile,
    logged_in: bool,
}

impl<R, W> MCClient<R, W> {
    pub fn new(
        reader: DecryptRead<R>,
        writer: EncryptedWriter<W>,
        connection_information: ConnectionInformation,
        username: String,
        profile: GameProfile,
    ) -> Self {
        Self {
            reader,
            writer: WrappedPacketWriter { writer }.lock(),
            compression_threshold: None,
            connection_information,
            username,
            profile,
            logged_in: false,
        }
    }
}

macro_rules! outsource_write_packet {
    () => {
        pub async fn write_packet<P: PacketComponent<(), ComponentType = P> + Send + Sync>(
            &self,
            packet: &P,
        ) -> drax::prelude::Result<()> {
            match self.compression_threshold {
                None => {
                    let mut lock = self.writer.write().await;
                    match lock.writer.write_packet(packet).await {
                        Ok(()) => {
                            drop(lock);
                            Ok(())
                        }
                        Err(e) => {
                            drop(lock);
                            Err(e)
                        }
                    }?;
                }
                Some(threshold) => match prepare_compressed_packet(threshold, packet).await? {
                    (size, None) => {
                        let mut lock = self.writer.write().await;
                        lock.writer.write_var_int(size + 1).await?;
                        lock.writer.write_var_int(0).await?;
                        lock.writer
                            .encode_component::<(), P>(&mut (), packet)
                            .await?;
                        drop(lock);
                    }
                    (size, Some(compressed)) => {
                        let mut lock = self.writer.write().await;
                        lock.writer
                            .write_var_int(
                                compressed.len() as i32 + size_var_int(size as i32) as i32,
                            )
                            .await?;
                        lock.writer.write_var_int(size).await?;
                        lock.writer.write_all(&compressed).await?;
                        drop(lock);
                    }
                },
            }
            Ok(())
        }
    };
}

impl<R, W> MCClient<R, W>
where
    R: AsyncRead + Unpin + Send + Sync,
{
    pub async fn read_packet<P: PacketComponent<()>>(
        &mut self,
    ) -> drax::prelude::Result<P::ComponentType> {
        match self.compression_threshold {
            None => self.reader.read_packet::<P>().await,
            // todo ensure read packet is sized correctly everywhere
            Some(_) => self.reader.read_compressed_packet::<P>().await,
        }
    }
}

impl<R, W> MCClient<R, W>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    pub async fn complete_login(&mut self) -> drax::prelude::Result<()> {
        if self.logged_in {
            throw_explain!("Client already logged in at this point.");
        }
        let mut lock = self.writer.write().await;
        lock.writer
            .write_packet(&LoginGameProfile {
                game_profile: self.profile.clone(),
            })
            .await?;
        drop(lock);
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
        let mut lock = self.writer.write().await;
        lock.writer
            .write_packet(&LoginCompression { threshold })
            .await?;
        lock.writer
            .write_compressed_packet(
                threshold,
                &LoginGameProfile {
                    game_profile: self.profile.clone(),
                },
            )
            .await?;
        drop(lock);
        self.logged_in = true;
        Ok(())
    }

    pub fn clone_writer(&self) -> StructuredWriterClone<W> {
        StructuredWriterClone {
            writer: self.writer.clone(),
            compression_threshold: self.compression_threshold,
        }
    }

    outsource_write_packet!();

    // Packet Helper Methods

    pub async fn disconnect<I: Into<Chat>>(&self, reason: I) -> drax::prelude::Result<()> {
        self.write_packet(&Disconnect {
            reason: reason.into(),
        })
        .await
    }
}

pub struct StructuredWriterClone<W> {
    writer: PacketWriter<EncryptedWriter<W>>,
    compression_threshold: Option<i32>,
}

impl<W> Clone for StructuredWriterClone<W> {
    fn clone(&self) -> Self {
        Self {
            writer: self.writer.clone(),
            compression_threshold: self.compression_threshold,
        }
    }
}

impl<W> StructuredWriterClone<W>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    pub fn new(
        writer: PacketWriter<EncryptedWriter<W>>,
        compression_threshold: Option<i32>,
    ) -> Self {
        Self {
            writer,
            compression_threshold,
        }
    }

    outsource_write_packet!();
}

pub struct ShovelClient {
    pub server_player: ServerPlayer,
}

impl ShovelClient {
    pub async fn send_client_login(
        &self,
        properties: ClientLoginProperties,
    ) -> drax::prelude::Result<()> {
        let ClientLoginProperties {
            player_id,
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
            .write_packet(&ClientLogin {
                player_id,
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
        self.server_player.write_packet(&KeepAlive { id: 0 }).await
    }

    pub async fn emit_brand<S: Into<String>>(&self, brand: S) -> drax::prelude::Result<()> {
        let mut brand_data = Cursor::new(Vec::new());
        String::encode(&brand.into(), &mut (), &mut brand_data).await?;
        self.server_player
            .write_packet(&CustomPayload {
                identifier: format!("minecraft:brand"),
                data: brand_data.into_inner(),
            })
            .await
    }

    pub async fn set_initial_position(
        &self,
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        player_id: i32,
    ) -> drax::prelude::Result<()> {
        self.server_player
            .write_packet(&SetDefaultSpawnPosition {
                pos: BlockPos {
                    x: x as i32,
                    y: y as i32,
                    z: z as i32,
                },
                angle: 0.0,
            })
            .await?;

        self.server_player
            .write_packet(&PlayerPosition {
                location: Location {
                    inner_loc: SimpleLocation { x, y, z },
                    yaw,
                    pitch,
                },
                relative_arguments: RelativeArgument::new(0x8),
                id: player_id,
                dismount: false,
            })
            .await
    }

    pub fn keep_alive(mut self) -> (GameProfile, UnboundedReceiver<ServerboundPlayRegistry>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let profile_clone = self.server_player.profile.clone();
        let cloned_writer = self.server_player.clone_writer();
        tokio::spawn(async move {
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
                        return;
                    }
                };
                if let ServerboundPlayRegistry::KeepAlive { keep_alive_id } = &packet {
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
                        return;
                    }
                } else {
                    if let Err(_) = tx.send(packet) {
                        return;
                    };
                }
            }
        });
        (profile_clone, rx)
    }
}
