//! breakwater — a tailnet reverse proxy.
//!
//! The binary (`main.rs`) wires these modules into listeners; they are exposed
//! as a library so integration tests can drive the proxy directly.

pub mod config;
pub mod proxy;
pub mod tls;
