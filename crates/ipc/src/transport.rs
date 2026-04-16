//! Shared frame reader/writer transport cores.

use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::codec::FrameCodec;
use crate::error::IpcError;
use tokio_util::codec::Decoder;

/// Steady-state capacity ceiling for per-connection frame buffers.
///
/// Without this ceiling, `BytesMut` grows monotonically — once a
/// connection handles a near-`MAX_FRAME_BYTES` burst, every idle
/// connection pays that high-water cost for the remainder of its
/// lifetime. Reclaiming capacity down to this ceiling bounds
/// steady-state RSS regardless of transient frame size spikes.
///
/// 64 KiB is comfortably above the common header-and-small-body case
/// and well below the `MAX_FRAME_BYTES` cap, so typical traffic does
/// not force reallocations and a single large burst does not
/// permanently pin memory.
pub(crate) const RETAINED_BUFFER_CAPACITY_CEILING: usize = 64 * 1024;

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

    #[cfg(test)]
    pub(crate) fn read_buf_capacity(&self) -> usize {
        self.read_buf.capacity()
    }
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub(crate) async fn recv_frame(&mut self) -> Result<Bytes, IpcError> {
        loop {
            if let Some(frame) = self.codec.decode(&mut self.read_buf)? {
                // Reclaim buffer capacity once we've completed a frame
                // AND no partial-frame bytes remain. The `is_empty`
                // precondition eliminates any copy hazard — if another
                // frame is already partially buffered, we leave the
                // capacity in place for the next `decode` call.
                maybe_reclaim(&mut self.read_buf);
                return Ok(frame);
            }

            let n = self.reader.read_buf(&mut self.read_buf).await?;
            if n == 0 {
                return match self.codec.decode_eof(&mut self.read_buf)? {
                    Some(frame) => {
                        maybe_reclaim(&mut self.read_buf);
                        Ok(frame)
                    }
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

    #[cfg(test)]
    pub(crate) fn frame_buf_capacity(&self) -> usize {
        self.frame_buf.capacity()
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
        // After the write completes, reclaim oversized write-side
        // capacity. We do not need to care about residual data because
        // `send_with` always clears `frame_buf` at the top of the next
        // send; reclaiming here keeps the effect visible to observers
        // that might sample capacity between sends.
        if self.frame_buf.capacity() > RETAINED_BUFFER_CAPACITY_CEILING {
            self.frame_buf = BytesMut::with_capacity(RETAINED_BUFFER_CAPACITY_CEILING);
        }
        Ok(())
    }
    pub(crate) async fn shutdown(&mut self) -> Result<(), IpcError> {
        self.writer.shutdown().await?;
        Ok(())
    }
}

/// Reclaim `buf` down to the retention ceiling if (and only if) the
/// buffer currently holds no partial frame bytes. The caller is
/// responsible for invoking this only at frame boundaries.
fn maybe_reclaim(buf: &mut BytesMut) {
    if buf.is_empty() && buf.capacity() > RETAINED_BUFFER_CAPACITY_CEILING {
        *buf = BytesMut::with_capacity(RETAINED_BUFFER_CAPACITY_CEILING);
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
