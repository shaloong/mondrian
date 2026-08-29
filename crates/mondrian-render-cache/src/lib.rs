//! Persistent Timeline render-cache contracts and bounded background service.
//!
//! This crate owns the deep cache boundary between semantic Timeline frame
//! evaluation and ordinary frame materialization. It deliberately knows
//! nothing about Preview scheduling or Export policy: callers provide a
//! complete content identity and immutable working-linear frame, then consume
//! only verified artifacts returned asynchronously.

mod artifact;
mod identity;
mod service;
mod store;

pub use artifact::{
    TimelineRenderCacheArtifactError, TimelineRenderCacheFrame,
    TimelineRenderCacheFrameValidationError,
};
pub use identity::{
    TimelineRenderCacheAlpha, TimelineRenderCacheFormat, TimelineRenderCacheIdentity,
};
pub use service::{
    TimelineRenderCacheConfig, TimelineRenderCacheDiagnostics, TimelineRenderCacheLookup,
    TimelineRenderCacheResult, TimelineRenderCacheService, TimelineRenderCacheSubmission,
};
