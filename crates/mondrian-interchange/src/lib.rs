//! Qualified timeline interchange for OTIO, CMX 3600, FCP 7 XML, and AAF.
//!
//! This crate is an app-independent Module. Format Adapters lower into one
//! exact private timeline representation; only the common materializer may
//! construct a [`mondrian_timeline::Sequence`]. Import never mutates author
//! state, and lossy export is rejected unless the caller explicitly admits the
//! machine-readable conformance report.

mod error;
mod export;
mod formats;
mod import;
mod limits;
mod model;
mod report;
mod toolchain;

pub use error::*;
pub use export::*;
pub use import::*;
pub use limits::*;
pub use model::*;
pub use report::*;
pub use toolchain::*;

/// Version of the public interchange request/report contract.
pub const INTERCHANGE_CONTRACT_VERSION: u32 = 1;
