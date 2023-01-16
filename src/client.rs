use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use drax::prelude::{DraxReadExt, DraxWriteExt, ErrorType, PacketComponent, Size};
use drax::transport::buffer::var_num::size_var_int;
use drax::transport::encryption::{Cipher, NewCipher};
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

    pub fn lock(self) -> PacketWriter<W> {
        Arc::new(RwLock::new(self))
    }

    pub fn into_inner(self) -> W {
        self.writer
    }

    pub fn write_and_cipher(&mut self) -> (&mut W, Option<&mut Cipher>) {
        (&mut self.writer, self.cipher.as_mut())
    }
}

pub type PacketWriter<W> = Arc<RwLock<WrappedPacketWriter<W>>>;

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
            P::encode(packet, &mut (), &mut buffer).await?;
            let mut buffer = buffer.into_inner();
            if let Some(cipher) = cipher {
                drax::transport::encryption::AsyncStreamCipher::encrypt(cipher, &mut buffer);
            }
            self.write_all(&buffer).await?;
            self.flush().await?;
            Ok(())
        })
    }
}

pub struct MCClient<R, W> {
    pub cipher: Option<Cipher>,
    pub reader: R,
    pub writer: PacketWriter<W>,
    compression_threshold: Option<i32>,
    pub connection_information: ConnectionInformation,
    pub username: String,
    pub profile: GameProfile,
    logged_in: bool,
}

impl<R, W> MCClient<R, W> {
    pub fn new(
        cipher_key: Option<&[u8]>,
        reader: R,
        writer: W,
        connection_information: ConnectionInformation,
        username: String,
        profile: GameProfile,
    ) -> Self {
        Self {
            cipher: cipher_key.map(|key| NewCipher::new_from_slices(key, key).unwrap()),
            reader,
            writer: WrappedPacketWriter {
                writer,
                cipher: cipher_key.map(|key| NewCipher::new_from_slices(key, key).unwrap()),
            }
            .lock(),
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
                    let (writer, cipher) = lock.write_and_cipher();
                    match writer.write_packet(cipher, packet).await {
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
                Some(_threshold) => {
                    todo!()
                }
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
            None => self.reader.read_packet::<P>(self.cipher.as_mut()).await,
            Some(_) => todo!(),
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
        let (writer, cipher) = lock.write_and_cipher();
        writer
            .write_packet(
                cipher,
                &LoginGameProfile {
                    game_profile: self.profile.clone(),
                },
            )
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
        let (writer, cipher) = lock.write_and_cipher();
        writer
            .write_packet(cipher, &LoginCompression { threshold })
            .await?;
        let (writer, cipher) = lock.write_and_cipher();
        writer
            .write_compressed_packet(
                cipher,
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
    writer: PacketWriter<W>,
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
    pub fn new(writer: PacketWriter<W>, compression_threshold: Option<i32>) -> Self {
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
