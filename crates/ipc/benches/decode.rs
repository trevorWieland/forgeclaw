use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use forgeclaw_core::JobId;
use forgeclaw_ipc::{
    ContainerToHost, OutputCompletePayload, OutputDeltaPayload, ReadyPayload, StopReason,
    decode_container_to_host,
};

fn known_frames() -> Vec<Vec<u8>> {
    let mut frames = Vec::new();

    let ready = ContainerToHost::Ready(ReadyPayload {
        adapter: "bench-adapter".parse().expect("valid adapter"),
        adapter_version: "1.0.0".parse().expect("valid adapter version"),
        protocol_version: "1.0".parse().expect("valid protocol version"),
    });
    frames.push(serde_json::to_vec(&ready).expect("serialize ready"));

    for size in [32usize, 1024, 8192, 65_536] {
        let delta = ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "x".repeat(size).parse().expect("valid text"),
            job_id: JobId::from("job-bench"),
        });
        frames.push(serde_json::to_vec(&delta).expect("serialize delta"));
    }

    let complete = ContainerToHost::OutputComplete(OutputCompletePayload {
        job_id: JobId::from("job-bench"),
        result: Some("done".parse().expect("valid result")),
        session_id: Some("sess-1".parse().expect("valid session id")),
        token_usage: None,
        stop_reason: StopReason::EndTurn,
    });
    frames.push(serde_json::to_vec(&complete).expect("serialize complete"));

    frames
}

fn bench_decode_known(c: &mut Criterion) {
    let frames = known_frames();
    let mut group = c.benchmark_group("decode_known");

    for frame in &frames {
        group.bench_with_input(
            BenchmarkId::from_parameter(frame.len()),
            frame,
            |b, bytes| {
                b.iter(|| {
                    let msg = decode_container_to_host(black_box(bytes)).expect("known message");
                    black_box(msg)
                });
            },
        );
    }

    group.finish();
}

fn bench_decode_unknown(c: &mut Criterion) {
    let bytes = br#"{"type":"unknown_future_feature","x":1}"#.to_vec();
    c.bench_function("decode_unknown_type", |b| {
        b.iter(|| {
            let err = decode_container_to_host(black_box(&bytes)).expect_err("unknown should fail");
            black_box(err)
        });
    });
}

fn bench_decode_malformed(c: &mut Criterion) {
    let bytes = br#"{"type":"output_delta","job_id":"job-bench"}"#.to_vec();
    c.bench_function("decode_malformed_known", |b| {
        b.iter(|| {
            let err =
                decode_container_to_host(black_box(&bytes)).expect_err("malformed should fail");
            black_box(err)
        });
    });
}

fn bench_decode_invalid_utf8(c: &mut Criterion) {
    let bytes = vec![0xFF, 0xFE, 0xFD];
    c.bench_function("decode_invalid_utf8", |b| {
        b.iter(|| {
            let err = decode_container_to_host(black_box(&bytes)).expect_err("utf8 should fail");
            black_box(err)
        });
    });
}

fn criterion_benchmark(c: &mut Criterion) {
    bench_decode_known(c);
    bench_decode_unknown(c);
    bench_decode_malformed(c);
    bench_decode_invalid_utf8(c);
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
