//! Structured IPC protocol over Unix domain sockets.
//!
//! `forgeclaw-ipc` is the host ↔ container protocol boundary. It is
//! the single most load-bearing interface in Forgeclaw — the stable
//! contract that every future container runtime, agent-runner binary,
//! and polyglot adapter (Rust, TypeScript, …) depends on.
//!
//! # Crate surface
//!
//! - [`message`] defines the two top-level protocol enums
//!   [`ContainerToHost`] and [`HostToContainer`] plus every payload
//!   struct, shared type, and closed enum they reference. Wire shape
//!   matches [`docs/IPC_PROTOCOL.md`](../../docs/IPC_PROTOCOL.md).
//! - [`codec::FrameCodec`] encodes and decodes 4-byte big-endian
//!   length-prefixed UTF-8 JSON frames with an explicit
//!   [`codec::MAX_FRAME_BYTES`] cap.
//! - [`server::IpcServer`] binds a Unix socket on the host side and
//!   accepts [`server::IpcConnection`]s.
//! - [`client::IpcClient`] connects to an [`server::IpcServer`] from
//!   the container side.
//! - [`error::IpcError`] (plus [`error::FrameError`] and
//!   [`error::ProtocolError`]) are the crate-local error taxonomy.
//! - [`version`] pins the local protocol version and provides a
//!   major-version compatibility check.
//!
//! # Handshake
//!
//! The lifecycle documented in
//! [`docs/IPC_PROTOCOL.md`](../../docs/IPC_PROTOCOL.md) §Lifecycle is:
//!
//! ```text
//! container → connect
//! container → ready
//! host      → init
//! (work happens)
//! host      → shutdown
//! container → output_complete
//! socket close
//! ```
//!
//! Both [`server::PendingConnection::handshake`] and
//! [`client::PendingClient::handshake`] encapsulate the Ready → Init
//! leg in a single call that consumes the pending type and returns the
//! established connection, so higher-level crates never have to reach
//! into the send/recv primitives just to establish a session.

pub mod client;
pub mod codec;
pub mod error;
pub mod message;
pub mod peer_cred;
pub(crate) mod policy;
pub mod server;
pub(crate) mod util;
pub mod version;

pub use client::{IpcClient, IpcClientReader, IpcClientWriter, PendingClient};
pub use codec::{
    FrameCodec, LENGTH_PREFIX_BYTES, MAX_FRAME_BYTES, decode_container_to_host,
    decode_host_to_container,
};
pub use error::{FrameError, IpcError, ProtocolError};
pub use message::{
    AuthorizedCommand, BranchPolicy, CancelTaskPayload, CommandBody, CommandPayload,
    ContainerToHost, DispatchSelfImprovementPayload, DispatchTanrenPayload, ErrorCode,
    ErrorPayload, GroupCapabilities, GroupCommand, GroupExtensions, GroupInfo, HeartbeatPayload,
    HistoricalMessage, HostToContainer, InitConfig, InitContext, InitPayload, MainGroupCommand,
    MessagesPayload, OutputCompletePayload, OutputDeltaPayload, OwnershipPending, PauseTaskPayload,
    Percent, PercentError, PrivilegedAuthorizedCommand, ProgressPayload, ReadyPayload,
    RegisterGroupPayload, ScheduleTaskPayload, ScheduleType, ScopedAuthorizedCommand,
    SendMessagePayload, ShutdownPayload, ShutdownReason, StopReason, TanrenPhase, TokenUsage,
};
pub use peer_cred::{PeerCredentials, SessionIdentity};
pub use server::{
    IpcConnection, IpcConnectionReader, IpcConnectionWriter, IpcServer, PendingConnection,
};
pub use version::{PROTOCOL_VERSION, is_compatible};
