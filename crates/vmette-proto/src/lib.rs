//! vmette-proto — the serde-only **wire contracts** shared across the vmette
//! workspace. This crate is a leaf (its only dependency is `serde`): it owns
//! the *types*, not any I/O. The framing codecs that carry these types live
//! with their transport — the vsock frame reader/writer in `vmette::desktop`,
//! the line-delimited JSON loop in `vmette-daemon`.
//!
//! Two contracts live here, one per submodule:
//!
//! * [`agent`] — the host↔guest **computer-use vocabulary**: the [`Action`]
//!   enum (mirroring the Anthropic computer-use tool) and the
//!   [`ResponseHeader`] the in-guest agent replies with. Spoken over vsock.
//! * [`daemon`] — the **`vmetted` UNIX-socket protocol**: the stateless run
//!   [`Request`](daemon::Request)/[`Frame`](daemon::Frame) pair and the
//!   stateful desktop [`DesktopRequest`](daemon::DesktopRequest)/
//!   [`DesktopReply`](daemon::DesktopReply) pair.
//!
//! [`geom::Rect`] is the one pixel-rectangle type both the perception layer and
//! the desktop replies share.
//!
//! Because every consumer (the core library, the daemon, the MCP server, the
//! CLI) depends on these *same* Rust types, a renamed field or a new variant is
//! a compile error at every site instead of a silent runtime wire break.

pub mod agent;
pub mod daemon;
pub mod geom;
pub mod mount;

pub use agent::{Action, ResponseHeader, ScrollDirection};
pub use geom::Rect;
pub use mount::ShareMount;
