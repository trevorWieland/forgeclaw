use super::{FrameReader, FrameWriter, RETAINED_BUFFER_CAPACITY_CEILING};
use crate::codec::encode_message_frame;
use crate::error::IpcError;
use bytes::BytesMut;
use tokio::io::{AsyncWriteExt, duplex};

#[tokio::test]
async fn reader_reclaims_capacity_after_large_frame() {
    // Simulate a peer that sends one large frame followed by silence:
    // the reader should drop excess capacity back to the ceiling
    // before returning the frame.
    let (mut peer, ours) = duplex(1 << 20);
    let payload = build_ready_frame_over_ceiling();
    peer.write_all(&payload).await.expect("write");
    peer.shutdown().await.expect("shutdown peer");

    let mut reader = FrameReader::new(ours);
    let frame = reader.recv_frame().await.expect("recv frame");
    assert!(!frame.is_empty());
    assert!(
        reader.read_buf_capacity() <= RETAINED_BUFFER_CAPACITY_CEILING,
        "reader capacity {} should be reclaimed to <= {}",
        reader.read_buf_capacity(),
        RETAINED_BUFFER_CAPACITY_CEILING,
    );
}

#[tokio::test]
async fn writer_reclaims_capacity_after_large_send() {
    // A single oversized send should not permanently pin buffer
    // capacity on the writer side either. The peer is drained in a
    // parallel task so write backpressure does not deadlock the test.
    let (ours, mut peer) = duplex(1 << 16);
    let drain = tokio::spawn(async move {
        let mut sink = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut peer, &mut sink)
            .await
            .expect("drain peer");
        sink.len()
    });

    let mut writer = FrameWriter::new(ours);
    writer
        .send_with(|buf| {
            let filler = vec![0xA5u8; RETAINED_BUFFER_CAPACITY_CEILING * 2];
            buf.extend_from_slice(
                &(u32::try_from(filler.len()).expect("len fits u32")).to_be_bytes(),
            );
            buf.extend_from_slice(&filler);
            Ok::<(), IpcError>(())
        })
        .await
        .expect("send oversized frame");

    assert!(
        writer.frame_buf_capacity() <= RETAINED_BUFFER_CAPACITY_CEILING,
        "writer capacity {} should be reclaimed to <= {}",
        writer.frame_buf_capacity(),
        RETAINED_BUFFER_CAPACITY_CEILING,
    );

    // Release the pipe so the drain task can finish.
    drop(writer);
    let drained = drain.await.expect("drain task joined");
    assert!(drained > RETAINED_BUFFER_CAPACITY_CEILING);
}

/// Build a single framed `ready` message whose buffer footprint
/// exceeds the retention ceiling by padding an oversize `adapter`
/// field (kept inside the bounded-text cap by reusing a variant-shaped
/// body of raw bytes).
fn build_ready_frame_over_ceiling() -> Vec<u8> {
    // Craft a payload that is structurally JSON but intentionally
    // bigger than the retention ceiling so the reader's buffer grows.
    // We wrap it in a protocol "heartbeat" stub via encode so the
    // framing is legal; the large size comes from repeating a
    // harmless field that the codec/decoder tolerates.
    let mut payload =
        String::from(r#"{"type":"heartbeat","timestamp":"2026-04-03T10:00:00Z","_padding":""#);
    payload.extend(std::iter::repeat_n(
        'x',
        RETAINED_BUFFER_CAPACITY_CEILING * 2,
    ));
    payload.push_str(r#""}"#);

    let mut framed = BytesMut::new();
    encode_message_frame(
        &serde_json::json!({
            "type": "heartbeat",
            "timestamp": "2026-04-03T10:00:00Z",
            "_padding": "x".repeat(RETAINED_BUFFER_CAPACITY_CEILING * 2),
        }),
        &mut framed,
    )
    .expect("encode framed padded payload");
    let _ = payload; // keep the ergonomic String above for readability
    framed.to_vec()
}
