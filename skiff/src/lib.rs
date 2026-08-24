//! skiffd — the agent desk (DW-004).
//!
//! One service that ingests harness sessions and fleet changes into a derived
//! read model, and serves that model to one React client as live queries over
//! a single socket.
//!
//! The organising idea, which every module below is a consequence of: **skiff
//! is a live read model over state that other processes own, plus a small set
//! of commands.** There are exactly two kinds of state — *observed* (someone
//! else authored it; always re-derivable; never the truth) and *authored*
//! (skiff authored it; append-only; irreplaceable) — and they get different
//! disciplines that never leak into each other.

pub mod config;
pub mod content;
pub mod ingest;
pub mod model;
pub mod run;
pub mod server;
pub mod store;
pub mod views;
pub mod wire;
