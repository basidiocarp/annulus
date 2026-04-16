//! Annulus library entry point.
//!
//! Exposes internal modules for integration testing. The binary at `main.rs`
//! is the primary consumer; this crate root exists so that `tests/` files can
//! import provider types directly.

pub mod providers;
pub mod usage;
