use tokio::time::Instant;

use crate::codec::decode_container_to_host;
use crate::error::IpcError;
use crate::lifecycle::LifecycleAction;
use crate::message::{ContainerToHost, HostToContainer};

use super::protocol::{ConnectionState, enforce_inbound_state, enforce_outbound_state};

pub(super) fn preflight_and_enforce_outbound(
    state: &mut ConnectionState,
    msg: &HostToContainer,
    now: Instant,
) -> Result<(), IpcError> {
    let _ = enforce_outbound_state(state, msg, now)?;
    Ok(())
}

pub(super) fn decode_and_enforce_inbound(
    state: &mut ConnectionState,
    frame: &[u8],
    now: Instant,
) -> Result<(ContainerToHost, LifecycleAction), IpcError> {
    let msg = decode_container_to_host(frame)?;
    let action = enforce_inbound_state(state, &msg, now)?;
    Ok((msg, action))
}
