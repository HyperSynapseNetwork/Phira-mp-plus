//! Official Phira client compatibility layer (PMP42).
//!
//! The official Phira client is an immutable compatibility target. PMP must
//! reproduce the observable behavior of the official `phira-mp` server. This
//! module implements the server-side side of that contract:
//!
//! - `response`: every official request-type `ClientCommand` receives the
//!   matching `ServerCommand` variant — rate-limit, permission and internal
//!   errors are never silently dropped (P0-A).
//! - `timing`: responses are delayed to a configurable minimum latency so the
//!   client's send→install-callback ordering is preserved, and the per-command
//!   actor budget is capped well below the client's ~7s deadline (P0-B/P0-C).
//! - `protocol_trace`: observability counters + latency histogram used to
//!   assert that silent response paths and late commits stay at zero (P1).
//! - `post_response`: ProtocolHack — PMP extension compensation messages
//!   (ChangeHost/ChangeState/Persistent Room/replay) are scheduled strictly
//!   after the official response flush, in a fixed order, without blocking the
//!   Room Actor (P1).

pub(crate) mod post_response;
pub(crate) mod protocol_trace;
pub(crate) mod response;
pub(crate) mod timing;
