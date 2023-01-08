use std::io::Cursor;
use std::sync::Arc;

use async_compression::tokio::write::{ZlibDecoder, ZlibEncoder};
use drax::prelude::{DraxReadExt, DraxWriteExt, PacketComponent, Size};
use drax::transport::buffer::var_num::size_var_int;
use drax::transport::encryption::{DecryptRead, EncryptedWriter};
use drax::{throw_explain, PinnedLivelyResult};
use mcprotocol::clientbound::login::ClientboundLoginRegistry::{
    LoginCompression, LoginGameProfile,
};
use mcprotocol::common::GameProfile;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::RwLock;

use crate::phase::ConnectionInformation;

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

impl<A: AsyncRead + Unpin> McPacketReader for A {
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

pub fn prepare_compressed_packet<P: PacketComponent<(), ComponentType = P>>(
    threshold: i32,
    packet: &P,
) -> PinnedLivelyResult<(i32, Option<Vec<u8>>)> {
    Box::pin(async move {
        let size = match P::size(packet, &mut ())? {
            Size::Dynamic(x) | Size::Constant(x) => x as i32,
        };
        if size >= threshold {
            let mut zlib_compressor = ZlibEncoder::new(Cursor::new(vec![]));
            zlib_compressor
                .encode_component::<(), P>(&mut (), packet)
                .await?;
            zlib_compressor.flush().await?;
            let prepared = zlib_compressor.into_inner().into_inner();
            Ok((size, Some(prepared)))
        } else {
            Ok((size, None))
        }
    })
}

pub trait McPacketWriter {
    fn write_compressed_packet<'a, P: PacketComponent<(), ComponentType = P>>(
        &'a mut self,
        threshold: i32,
        packet: &'a P,
    ) -> PinnedLivelyResult<'a, ()>
    where
        Self: AsyncWrite + Unpin,
    {
        Box::pin(async move {
            let (packet_size, compressed) = prepare_compressed_packet(threshold, packet).await?;
            if let Some(compressed) = compressed {
                self.write_var_int(
                    compressed.len() as i32 + size_var_int(packet_size as i32) as i32,
                )
                .await?;
                self.write_var_int(packet_size).await?;
                self.write_all(&compressed).await?;
            } else {
                self.write_var_int(packet_size + 1).await?;
                self.write_var_int(0).await?;
                self.encode_component::<(), P>(&mut (), packet).await?;
            }
            Ok(())
        })
    }

    fn write_packet<'a, P: PacketComponent<(), ComponentType = P>>(
        &'a mut self,
        packet: &'a P,
    ) -> PinnedLivelyResult<'a, ()>;

    fn write_with_ref<'a, P: PacketComponent<(), ComponentType = P>>(
        &'a self,
        _packet: &'a P,
    ) -> PinnedLivelyResult<'a, ()> {
        unimplemented!("write_with_ref not implemented for this type")
    }

    fn write_compressed_with_ref<'a, P: PacketComponent<(), ComponentType = P>>(
        &'a self,
        _threshold: i32,
        _packet: &'a P,
    ) -> PinnedLivelyResult<'a, ()> {
        unimplemented!("write_compressed_with_ref not implemented for this type")
    }
}

impl<A: AsyncWrite + Unpin> McPacketWriter for A {
    fn write_packet<'a, P: PacketComponent<(), ComponentType = P>>(
        &'a mut self,
        packet: &'a P,
    ) -> PinnedLivelyResult<'a, ()> {
        Box::pin(async move {
            self.write_var_int(match P::size(packet, &mut ())? {
                Size::Dynamic(x) | Size::Constant(x) => x as i32,
            })
            .await?;
            P::encode(packet, &mut (), self).await?;
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
        pub async fn write_packet<P: PacketComponent<(), ComponentType = P>>(
            &self,
            packet: &P,
        ) -> drax::prelude::Result<()> {
            match self.compression_threshold {
                None => {
                    let mut lock = self.writer.write().await;
                    lock.writer.write_packet(packet).await?;
                    drop(lock);
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
    R: AsyncRead + Unpin,
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
    W: AsyncWrite + Unpin,
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
}

pub struct StructuredWriterClone<W> {
    writer: PacketWriter<EncryptedWriter<W>>,
    compression_threshold: Option<i32>,
}

impl<W> StructuredWriterClone<W>
where
    W: AsyncWrite + Unpin,
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
