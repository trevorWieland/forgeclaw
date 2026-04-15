//! Shared client-side protocol lifecycle enforcement.

use forgeclaw_core::JobId;

use crate::error::IpcError;
use crate::lifecycle::{
    LifecycleAction, LifecycleState, enforce_container_to_host, enforce_host_to_container,
};
use crate::message::{ContainerToHost, HostToContainer};

/// Per-client runtime protocol state.
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
