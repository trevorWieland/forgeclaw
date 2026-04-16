use std::io::Write;
use std::sync::{Arc, Mutex};

use forgeclaw_core::{GroupId, JobId};
use tokio::time::Instant;
use tracing_subscriber::fmt::MakeWriter;

use super::{
    ConnectionState, log_fatal_protocol_error, log_unauthorized_command, log_unknown_message,
    record_unauthorized_rejection,
};
use crate::error::{FrameError, IpcError, ProtocolError};
use crate::message::shared::{GroupCapabilities, GroupInfo};
use crate::peer_cred::SessionIdentity;
use crate::policy::{
    UNKNOWN_LOG_BURST, UNKNOWN_LOG_EVERY, UnknownTrafficBudget, UnknownTrafficLimitConfig,
};
use crate::server::UnauthorizedCommandLimitConfig;
use crate::util::sampler::SampledCounter;
use crate::version::{PROTOCOL_VERSION, negotiate};

fn parse_job_id(value: &str) -> JobId {
    JobId::new(value).expect("valid job id")
}

fn state_with_job(now: Instant, job_id: &str) -> ConnectionState {
    ConnectionState::new(
        now,
        parse_job_id(job_id),
        negotiate(PROTOCOL_VERSION).expect("local version must negotiate"),
        UnauthorizedCommandLimitConfig::default(),
        Some(std::time::Duration::from_secs(60)),
    )
}

#[test]
fn unauthorized_logging_emits_audit_per_attempt_and_sampled_warns() {
    let capture = CaptureWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .without_time()
        .with_writer(capture.clone())
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let identity = sample_identity();
    let mut state = state_with_job(Instant::now(), "job-1");

    tracing::dispatcher::with_default(&dispatch, || {
        for _ in 0..7 {
            let decision = record_unauthorized_rejection(&mut state).log;
            log_unauthorized_command(
                &identity,
                "processing",
                "register_group",
                "requires main-group privilege",
                decision,
            );
        }
    });

    let output = capture.output();
    assert_eq!(
        output
            .matches("audit unauthorized IPC command rejection")
            .count(),
        7
    );
    assert_eq!(
        output.matches("rejected unauthorized IPC command").count(),
        5
    );
}

#[derive(Clone, Default)]
struct CaptureWriter {
    inner: Arc<Mutex<Vec<u8>>>,
}

impl CaptureWriter {
    fn output(&self) -> String {
        let bytes = self.inner.lock().expect("lock").clone();
        String::from_utf8(bytes).expect("utf8 log output")
    }
}

struct CaptureGuard {
    inner: Arc<Mutex<Vec<u8>>>,
}

impl Write for CaptureGuard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.lock().expect("lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureGuard;

    fn make_writer(&'a self) -> Self::Writer {
        CaptureGuard {
            inner: Arc::clone(&self.inner),
        }
    }
}

fn sample_identity() -> Arc<SessionIdentity> {
    Arc::new(SessionIdentity::new(
        None,
        GroupInfo {
            id: GroupId::new("group-main").expect("valid group id"),
            name: "Main".parse().expect("valid name"),
            is_main: true,
            capabilities: GroupCapabilities::default(),
        },
    ))
}

#[test]
fn fatal_logging_includes_identity_and_error_class() {
    let capture = CaptureWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .without_time()
        .with_writer(capture.clone())
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let identity = sample_identity();
    tracing::dispatcher::with_default(&dispatch, || {
        log_fatal_protocol_error(
            &identity,
            "handshake",
            &IpcError::Frame(FrameError::InvalidUtf8),
        );
    });
    let output = capture.output();
    assert!(
        output.contains("\"protocol_phase\":\"handshake\""),
        "{output}"
    );
    assert!(
        output.contains("\"error_class\":\"frame_invalid_utf8\""),
        "{output}"
    );
    assert!(output.contains("\"group_id\":\"group-main\""), "{output}");
}

#[test]
fn fatal_logging_truncates_peer_controlled_protocol_version() {
    let capture = CaptureWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .without_time()
        .with_writer(capture.clone())
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let identity = sample_identity();
    let long_peer = "x".repeat(8_000);
    let err = IpcError::Protocol(ProtocolError::UnsupportedVersion {
        peer: long_peer.clone(),
        local: "1.0",
    });

    tracing::dispatcher::with_default(&dispatch, || {
        log_fatal_protocol_error(&identity, "handshake", &err);
    });

    let output = capture.output();
    assert!(output.contains("<truncated"), "{output}");
    assert!(
        !output.contains(&long_peer),
        "full peer value leaked to logs"
    );
}

#[test]
fn unknown_message_logging_emits_audit_per_attempt_and_sampled_warns() {
    let capture = CaptureWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .without_time()
        .with_writer(capture.clone())
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let identity = sample_identity();

    let now = Instant::now();
    let mut budget = UnknownTrafficBudget::new(now, UnknownTrafficLimitConfig::default());
    let mut sampler = SampledCounter::new(UNKNOWN_LOG_BURST, UNKNOWN_LOG_EVERY);

    tracing::dispatcher::with_default(&dispatch, || {
        for _ in 0..12 {
            budget.on_unknown(16, now).expect("budget accepts unknown");
            let decision = sampler.observe();
            log_unknown_message(&identity, "future_message_type", &budget, decision);
        }
    });

    let output = capture.output();
    // Every observed attempt emits the audit/debug line.
    assert_eq!(output.matches("audit unknown IPC message skip").count(), 12);
    // Burst of 5 + every-10th = 5 (attempts 1..=5) + 1 (attempt 10) = 6.
    assert_eq!(output.matches("ignoring unknown message type").count(), 6);
}

#[test]
fn fatal_logging_maps_idle_read_timeout_error_class() {
    let capture = CaptureWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .without_time()
        .with_writer(capture.clone())
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let identity = sample_identity();

    tracing::dispatcher::with_default(&dispatch, || {
        log_fatal_protocol_error(
            &identity,
            "idle",
            &IpcError::Protocol(ProtocolError::IdleReadTimeout {
                phase: "idle",
                timeout_secs: 60,
            }),
        );
    });

    let output = capture.output();
    assert!(
        output.contains("\"error_class\":\"idle_read_timeout\""),
        "{output}"
    );
}
