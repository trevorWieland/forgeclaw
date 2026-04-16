//! Shared client-side protocol lifecycle enforcement.

use forgeclaw_core::JobId;

use crate::error::IpcError;
use crate::lifecycle::{
    LifecycleAction, LifecycleState, enforce_container_to_host, enforce_host_to_container,
};
use crate::message::{ContainerToHost, HostToContainer};

/// Per-client runtime protocol state.
///
/// # Concurrency invariants
///
/// The field is guarded by a single
/// [`AsyncMutex`](tokio::sync::Mutex) held by the owning
/// [`crate::client::IpcClient`] (and its split reader/writer halves).
/// The same ordering rules as the server-side state apply:
///
/// 1. **Linear phase progression.** `lifecycle` transitions are only
///    advanced from inside the mutex so no observer sees the phase
///    regress.
/// 2. **Poisoning is split.** The `poisoned` `AtomicBool` on the
///    owning reader/writer is authoritative and does NOT require the
///    mutex to read; a `true` poisoned flag short-circuits send/recv
///    paths before they ever acquire this lock.
///
/// Outbound state validation ([`validate_outbound_state`]) clones
/// `lifecycle` before attempting enforcement so validation under a
/// read-style lock never mutates the shared phase. Callers that need
/// to commit a transition use [`enforce_outbound_state`] / [`enforce_inbound_state`]
/// with exclusive access.
#[derive(Debug, Clone)]
pub(super) struct ClientConnectionState {
    lifecycle: LifecycleState,
}

impl ClientConnectionState {
    pub(super) fn new(active_job_id: JobId) -> Self {
        Self {
            lifecycle: LifecycleState::new(active_job_id),
        }
    }
}

pub(super) fn enforce_outbound_state(
    state: &mut ClientConnectionState,
    msg: &ContainerToHost,
) -> Result<LifecycleAction, IpcError> {
    enforce_container_to_host(&mut state.lifecycle, msg)
}

pub(super) fn validate_outbound_state(
    state: &ClientConnectionState,
    msg: &ContainerToHost,
) -> Result<(), IpcError> {
    let mut lifecycle = state.lifecycle.clone();
    let _ = enforce_container_to_host(&mut lifecycle, msg)?;
    Ok(())
}

pub(super) fn enforce_inbound_state(
    state: &mut ClientConnectionState,
    msg: &HostToContainer,
) -> Result<LifecycleAction, IpcError> {
    enforce_host_to_container(&mut state.lifecycle, msg)
}
