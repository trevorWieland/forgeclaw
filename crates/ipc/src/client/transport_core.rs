use crate::codec::decode_host_to_container;
use crate::error::IpcError;
use crate::lifecycle::LifecycleAction;
use crate::message::{ContainerToHost, HostToContainer};

use super::protocol::{ClientConnectionState, enforce_inbound_state, enforce_outbound_state};

pub(super) fn preflight_and_enforce_outbound(
    state: &mut ClientConnectionState,
    msg: &ContainerToHost,
) -> Result<LifecycleAction, IpcError> {
    let action = enforce_outbound_state(state, msg)?;
    Ok(action)
}

pub(super) fn decode_and_enforce_inbound(
    state: &mut ClientConnectionState,
    frame: &[u8],
) -> Result<(HostToContainer, LifecycleAction), IpcError> {
    let msg = decode_host_to_container(frame)?;
    let action = enforce_inbound_state(state, &msg)?;
    Ok((msg, action))
}
