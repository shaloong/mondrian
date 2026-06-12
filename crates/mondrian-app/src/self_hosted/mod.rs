//! Self-hosted UI application shell and panel adapters.
//!
//! This module is the bridge between the reusable `mondrian-ui-*` crates and
//! the Mondrian application layer. Binaries should stay thin and call into this
//! module instead of accumulating panel or runtime wiring.

pub mod panels;
pub mod runtime;
