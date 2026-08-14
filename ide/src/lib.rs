//! The fleet IDE's core: everything both binaries share. Deliberately
//! gpui-free — the `ide` GUI wraps this with the shell (its bin-private
//! modules), and `ide-server` serves it headless over JSON-RPC
//! (docs/remote.md). Single-package note: the server *build* still compiles
//! the package's GUI dependencies, but the binary links none of them; a
//! core-crate split is the follow-up if server-only builds ever matter.

pub mod documents;
pub mod lsp;
pub mod remote;
pub mod rpc;
pub mod server;
pub mod workspace;
