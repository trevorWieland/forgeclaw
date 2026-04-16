//! Buffer retention regression benchmark.
//!
//! The point of this benchmark is not to measure raw throughput
//! (that's `decode.rs`) but to surface regressions in the buffer
//! reclaim logic. A single large frame followed by small frames
//! should NOT permanently pin capacity.

use bytes::BytesMut;
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use forgeclaw_ipc::{FrameCodec, MAX_FRAME_BYTES};
use tokio_util::codec::{Decoder, Encoder};

const BIG_PAYLOAD: usize = 1 << 20; // 1 MiB
const SMALL_PAYLOAD: usize = 32;

fn framed_bytes(size: usize) -> BytesMut {
    let mut codec = FrameCodec::new();
    let mut out = BytesMut::new();
    codec
        .encode(vec![0xA5u8; size].into(), &mut out)
        .expect("encode");
    out
}

fn bench_large_then_small_cycles(c: &mut Criterion) {
    c.bench_function("burst_then_idle_cycle", |b| {
        b.iter(|| {
            let mut codec = FrameCodec::new();
            let mut reader_buf = BytesMut::new();

            reader_buf.extend_from_slice(&framed_bytes(BIG_PAYLOAD));
            let big = codec.decode(&mut reader_buf).expect("decode big");
            black_box(big);

            for _ in 0..16 {
                reader_buf.extend_from_slice(&framed_bytes(SMALL_PAYLOAD));
                let small = codec.decode(&mut reader_buf).expect("decode small");
                black_box(small);
            }
        });
    });
}

fn bench_max_frame_reject(c: &mut Criterion) {
    // The oversized-length path should never allocate the declared
    // payload — this benchmark captures the cost of the reject-fast
    // branch so regressions that accidentally buffer the payload
    // show up immediately.
    c.bench_function("max_frame_reject_fast", |b| {
        b.iter(|| {
            let mut codec = FrameCodec::new();
            // 0x00_FF_00_00 = 16_711_680 bytes > MAX_FRAME_BYTES
            let _ = MAX_FRAME_BYTES;
            let mut buf = BytesMut::from(&[0x00u8, 0xFF, 0x00, 0x00][..]);
            let err = codec
                .decode(black_box(&mut buf))
                .expect_err("oversize length must reject");
            black_box(err);
        });
    });
}

fn criterion_benchmark(c: &mut Criterion) {
    bench_large_then_small_cycles(c);
    bench_max_frame_reject(c);
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
