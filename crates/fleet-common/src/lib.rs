//! Shared scaffolding for fleet services.
//!
//! Every fleet service used to carry a private copy of the same ~100 lines:
//! the SQLite open/migrate dance, the `{"error": …}` JSON responder, the SPA
//! static-file setup, and the env/path helpers. Copies drift — one service's
//! copy of the migration runner lost the foreign-keys-off bracket that another
//! service's data integrity depended on. This crate owns those invariants in
//! exactly one place.
//!
//! It is deliberately a toolbox, not a framework: services keep their own
//! routers, domain stores, and domain error variants, and pull in the pieces
//! that apply.

pub mod http;
pub mod store;
pub mod util;

pub use http::Error;
pub use http::Result;
