use crate::codec::decode_host_to_container_with_tally;
use crate::error::IpcError;
use crate::forward_compat::IgnoredFieldTally;
use crate::lifecycle::LifecycleAction;
use crate::message::{ContainerToHost, HostToContainer};

use super::protocol::{
    ClientConnectionState, enforce_inbound_state, enforce_outbound_state, validate_outbound_state,
};

pub(super) fn prepare_outbound(
    state: &ClientConnectionState,
    msg: &ContainerToHost,
) -> Result<(), IpcError> {
    validate_outbound_state(state, msg)
}

pub(super) fn commit_outbound(
    state: &mut ClientConnectionState,
    msg: &ContainerToHost,
) -> Result<LifecycleAction, IpcError> {
    enforce_outbound_state(state, msg)
}

pub(super) fn decode_and_enforce_inbound(
    state: &mut ClientConnectionState,
    frame: &[u8],
) -> Result<(HostToContainer, IgnoredFieldTally, LifecycleAction), IpcError> {
    let (msg, tally) = decode_host_to_container_with_tally(frame)?;
    let action = enforce_inbound_state(state, &msg)?;
    Ok((msg, tally, action))
}
