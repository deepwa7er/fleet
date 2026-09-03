//! keep — the fleet's central database service.
//!
//! The binary (`main.rs`) is thin CLI and config; everything here is the
//! library so services can embed a real keep in tests (recipes does) instead
//! of mocking the wire contract.

pub mod backup;
pub mod server;
pub mod store;
