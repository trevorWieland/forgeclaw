//! Shared frame reader/writer transport cores.

use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::codec::FrameCodec;
use crate::error::IpcError;
use tokio_util::codec::Decoder;

/// Read side for length-prefixed JSON frames.
#[derive(Debug)]
pub(crate) struct FrameReader<R> {
    reader: R,
    codec: FrameCodec,
    read_buf: BytesMut,
}

impl<R> FrameReader<R> {
    pub(crate) fn new(reader: R) -> Self {
        Self {
            reader,
            codec: FrameCodec::new(),
            read_buf: BytesMut::new(),
        }
    }
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub(crate) async fn recv_frame(&mut self) -> Result<Bytes, IpcError> {
        loop {
            if let Some(frame) = self.codec.decode(&mut self.read_buf)? {
                return Ok(frame);
            }

            let n = self.reader.read_buf(&mut self.read_buf).await?;
            if n == 0 {
                return match self.codec.decode_eof(&mut self.read_buf)? {
                    Some(frame) => Ok(frame),
                    None => Err(IpcError::Closed),
                };
            }
        }
    }
}

/// Write side for length-prefixed JSON frames.
#[derive(Debug)]
pub(crate) struct FrameWriter<W> {
    writer: W,
    frame_buf: BytesMut,
}

impl<W> FrameWriter<W> {
    pub(crate) fn new(writer: W) -> Self {
        Self {
            writer,
            frame_buf: BytesMut::new(),
        }
    }
}

impl<W: AsyncWrite + Unpin> FrameWriter<W> {
    pub(crate) async fn send_with(
        &mut self,
        encode: impl FnOnce(&mut BytesMut) -> Result<(), IpcError>,
    ) -> Result<(), IpcError> {
        self.frame_buf.clear();
        encode(&mut self.frame_buf)?;
        self.writer.write_all(&self.frame_buf).await?;
        self.writer.flush().await?;
        Ok(())
    }
    pub(crate) async fn shutdown(&mut self) -> Result<(), IpcError> {
        self.writer.shutdown().await?;
        Ok(())
    }
}
