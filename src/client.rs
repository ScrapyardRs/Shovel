use async_compression::tokio::write::{ZlibDecoder, ZlibEncoder};
use std::future::Future;
use std::io::Cursor;
use std::pin::Pin;
use std::sync::Arc;

use crate::phase::ConnectionInformation;
use drax::prelude::{DraxReadExt, DraxWriteExt, PacketComponent, Size};
use drax::throw_explain;
use drax::transport::buffer::var_num::size_var_int;
use drax::transport::encryption::{DecryptRead, EncryptedWriter};
use mcprotocol::clientbound::login::ClientboundLoginRegistry::LoginCompression;
use mcprotocol::common::GameProfile;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::RwLock;

pub struct WrappedPacketWriter<W> {
    pub writer: W,
    pub compression_threshold: Option<i32>,
}

impl<W> WrappedPacketWriter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            compression_threshold: None,
        }
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
    fn read_packet<'a, P: PacketComponent<()>>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = drax::prelude::Result<P::ComponentType>> + 'a>>;

    fn read_compressed_packet<'a, P: PacketComponent<()>>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = drax::prelude::Result<P::ComponentType>> + 'a>>;
}

impl<A: AsyncRead + Unpin> McPacketReader for A {
    fn read_packet<'a, P: PacketComponent<()>>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = drax::prelude::Result<P::ComponentType>> + 'a>> {
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

    fn read_compressed_packet<'a, P: PacketComponent<()>>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = drax::prelude::Result<P::ComponentType>> + 'a>> {
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

pub trait McPacketWriter {
    fn prepare_compressed_packet<'a, P: PacketComponent<(), ComponentType = P>>(
        threshold: i32,
        packet: &'a P,
    ) -> Pin<Box<dyn Future<Output = drax::prelude::Result<(i32, Option<Vec<u8>>)>> + 'a>>;

    fn write_compressed_packet<'a, P: PacketComponent<(), ComponentType = P>>(
        &'a mut self,
        threshold: i32,
        packet: &'a P,
    ) -> Pin<Box<dyn Future<Output = drax::prelude::Result<()>> + 'a>>
    where
        Self: AsyncWrite + Unpin,
    {
        Box::pin(async move {
            let (packet_size, compressed) =
                Self::prepare_compressed_packet(threshold, packet).await?;
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
    ) -> Pin<Box<dyn Future<Output = drax::prelude::Result<()>> + 'a>>;
}

impl<A: AsyncWrite + Unpin> McPacketWriter for A {
    fn prepare_compressed_packet<'a, P: PacketComponent<(), ComponentType = P>>(
        threshold: i32,
        packet: &'a P,
    ) -> Pin<Box<dyn Future<Output = drax::prelude::Result<(i32, Option<Vec<u8>>)>> + 'a>> {
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

    fn write_packet<'a, P: PacketComponent<(), ComponentType = P>>(
        &'a mut self,
        packet: &'a P,
    ) -> Pin<Box<dyn Future<Output = drax::prelude::Result<()>> + 'a>> {
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
            writer: WrappedPacketWriter {
                writer,
                compression_threshold: None,
            }
            .lock(),
            compression_threshold: None,
            connection_information,
            username,
            profile,
        }
    }

    pub async fn compress(&mut self, threshold: i32) -> drax::transport::Result<()>
    where
        W: AsyncWrite + Unpin,
    {
        self.compression_threshold = Some(threshold);
        let mut lock = self.writer.write().await;
        lock.writer
            .write_packet(&LoginCompression { threshold })
            .await?;
        lock.compression_threshold = Some(threshold);
        drop(lock);
        Ok(())
    }
}
