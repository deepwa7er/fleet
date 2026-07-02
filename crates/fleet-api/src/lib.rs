//! Cross-service API contracts: response shapes that one fleet service
//! produces and another consumes, defined once so both sides compile against
//! the same type. Before this crate, spyglass hand-mirrored its upstreams'
//! JSON — a benign field rename upstream turned a whole search source into
//! "bad response" with nothing to catch it.
//!
//! Only contracts whose producer AND consumer live in this workspace belong
//! here. External producers (lagoon's note search) stay hand-mirrored at the
//! consumer, clearly marked — a shared type the producer doesn't compile
//! against would be false confidence, not a contract.

pub mod search;
