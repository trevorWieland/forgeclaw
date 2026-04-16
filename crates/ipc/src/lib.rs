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
//!
//! # Security Boundaries
//!
//! - **Fail-closed socket path**: both [`server::IpcServer`] and
//!   [`client::IpcClient`] apply a shared lexical policy — absolute
//!   paths only, no `.`/`..` traversal, no unsupported prefix. The
//!   server layers its additional directory-metadata checks (mode
//!   0o700, symlink chain) on top; the client additionally rejects
//!   non-socket targets before attempting `connect`.
//! - **Peer credentials**: accepted peers always have credentials
//!   captured (when available). [`server::IpcServer::bind`] defaults
//!   to [`PeerCredentialPolicy::MatchCapturedProcess`], snapping the
//!   host's UID/GID at bind time and rejecting mismatched peers at
//!   accept. [`IpcServerOptions::hardened`] lets callers pass explicit
//!   UID/GID; [`IpcServerOptions::insecure_capture_only`] is the only
//!   audit-visible path to permissive mode.
//! - **Liveness semantics**: processing heartbeat timeout is extended by
//!   inbound container liveness (`heartbeat`) only; host outbound
//!   follow-up `messages` do not refresh the heartbeat deadline.
//!
//! # Domain-Specific Wire Types
//!
//! The crate exposes per-field newtypes (`AdapterName`, `AdapterVersion`,
//! `ProtocolVersionText`, `StageName`, `SenderName`, `GroupName`,
//! `ProjectName`, `BranchName`, `ContextModeText`,
//! `EnvironmentProfileText`) so semantically distinct 128-character
//! fields are compile-time distinguishable. `ProtocolVersionText` and
//! `BranchName` layer format validators on top of the length bound;
//! the remaining types enforce length only.

pub mod client;
pub mod codec;
pub mod error;
pub(crate) mod lifecycle;
pub mod message;
pub(crate) mod outbound_validation;
pub(crate) mod path_policy;
pub mod peer_cred;
pub(crate) mod policy;
pub mod recv_policy;
pub(crate) mod semantics;
pub mod server;
pub(crate) mod transport;
pub(crate) mod util;
pub mod version;

pub use client::{IpcClient, IpcClientOptions, IpcClientReader, IpcClientWriter, PendingClient};
pub use codec::{
    FrameCodec, LENGTH_PREFIX_BYTES, MAX_FRAME_BYTES, decode_container_to_host,
    decode_host_to_container,
};
pub use error::{FrameError, IpcError, ProtocolError};
pub use message::{
    AdapterName, AdapterVersion, AuthorizedCommand, BoundedCollectionError, BranchName,
    BranchPolicy, CancelTaskPayload, CommandBody, CommandPayload, ContainerToHost, ContextModeText,
    DispatchSelfImprovementPayload, DispatchTanrenPayload, EnvironmentProfileText, ErrorCode,
    ErrorPayload, GroupCapabilities, GroupCommand, GroupExtensions, GroupExtensionsError,
    GroupExtensionsVersion, GroupExtensionsVersionError, GroupInfo, GroupName, HeartbeatPayload,
    HistoricalMessage, HistoricalMessages, HostToContainer, IanaTimezone, InitConfig, InitContext,
    InitPayload, IpcTimestamp, MainGroupCommand, MessagesPayload, OutputCompletePayload,
    OutputDeltaPayload, OwnershipPending, PauseTaskPayload, Percent, PercentError,
    PrivilegedAuthorizedCommand, ProgressPayload, ProjectName, ProtocolVersionText, ReadyPayload,
    RegisterGroupPayload, ScheduleTaskPayload, ScheduleType, ScopedAuthorizedCommand,
    SelfImprovementListItems, SendMessagePayload, SenderName, ShutdownPayload, ShutdownReason,
    StageName, StopReason, TanrenPhase, TimestampError, TimezoneError, TokenUsage,
};
pub use peer_cred::{PeerCredentials, SessionIdentity};
pub use recv_policy::UnknownTypePolicy;
pub use server::{
    IpcConnection, IpcConnectionReader, IpcConnectionWriter, IpcInboundEvent, IpcServer,
    IpcServerOptions, PeerCredentialPolicy, PeerCredentialPolicyError, PendingConnection,
    UnauthorizedCommandLimitConfig, UnauthorizedCommandRejection, UnknownTrafficLimitConfig,
};
pub use version::{NegotiatedProtocolVersion, PROTOCOL_VERSION, is_compatible, negotiate};
