//! Line-survival engine, identity resolution and work-type classification.

pub mod engine;
pub mod identity;
pub mod repo;

/// Re-exported so callers use the same gix version as the engine.
pub use gix;
