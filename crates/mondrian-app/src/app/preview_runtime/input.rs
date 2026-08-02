//! Immutable input contract for one production Preview execution turn.
//!
//! The values in this module borrow canonical authoring state only for the
//! duration of one call. They are not retained by the Runtime and never become
//! a second authoring or Playback authority.

use std::collections::BTreeSet;
use std::time::Instant;

use mondrian_assets::AssetLibrary;
use mondrian_core::{types::AssetId, FrameRounding};
use mondrian_core::{
    DisplayManagementPolicy, FramePosition, ProjectColorEnvironment, ProjectSettings,
};
use mondrian_editor_state::AuthoringSessionId;
use mondrian_media::ProxyConfig;
use mondrian_playback::{
    FrameDemand, FrameDemandIdentity, FramePresentationQuality, FramePresentationTicket,
    PlaybackEpoch, PreviewResolutionScale, TransportState,
};
use mondrian_renderer::PreparedVisualAuthorSnapshotIdentity;
use mondrian_timeline::sequence::{Sequence, SequenceCollection};

use crate::app::playback_preview::PreviewTransportIntent;
use crate::app::preview_media_source::PreviewProxyGenerationIntent;
use crate::app::proxy_generation::ProxyGenerationRequestOutcome;
use crate::app::ui_actions::TimelineSeekSource;

/// Narrow command seam used when Preview source resolution discovers missing
/// or stale proxy work.
///
/// The immutable execution snapshot never contains this command authority.
pub(crate) trait PreviewProxyDemandSink {
    /// Submit one exact proxy demand to its domain-owned execution module.
    fn request_preview_proxy(
        &self,
        intent: PreviewProxyGenerationIntent,
    ) -> ProxyGenerationRequestOutcome;
}

/// Project proxy-selection facts captured for one Preview execution turn.
pub(crate) struct PreviewProxySelectionSnapshot<'a> {
    enabled: bool,
    forced_assets: &'a BTreeSet<AssetId>,
    config: ProxyConfig,
}

impl<'a> PreviewProxySelectionSnapshot<'a> {
    /// Capture the exact Project proxy policy used by this turn.
    pub(crate) fn new(
        settings: &ProjectSettings,
        forced_assets: &'a BTreeSet<AssetId>,
        config: ProxyConfig,
    ) -> Self {
        Self {
            enabled: settings.proxy_enabled,
            forced_assets,
            config,
        }
    }

    /// Whether this Asset is authored to prefer a proxy.
    pub(crate) fn prefers_proxy(&self, asset_id: AssetId) -> bool {
        self.enabled && self.forced_assets.contains(&asset_id)
    }

    /// Concrete proxy artifact configuration resolved by the App composition root.
    pub(crate) fn config(&self) -> &ProxyConfig {
        &self.config
    }
}

/// Borrowed, internally coherent authoring graph required by Preview.
pub(crate) struct PreviewAuthoringSnapshot<'a> {
    session_id: AuthoringSessionId,
    author_generation: u64,
    sequences: &'a SequenceCollection,
    color_environment: &'a ProjectColorEnvironment,
    asset_library: &'a AssetLibrary,
    proxy: PreviewProxySelectionSnapshot<'a>,
}

impl<'a> PreviewAuthoringSnapshot<'a> {
    /// Capture one validated open Authoring Session.
    pub(crate) fn new(
        session_id: AuthoringSessionId,
        author_generation: u64,
        sequences: &'a SequenceCollection,
        color_environment: &'a ProjectColorEnvironment,
        asset_library: &'a AssetLibrary,
        proxy: PreviewProxySelectionSnapshot<'a>,
    ) -> Self {
        Self {
            session_id,
            author_generation,
            sequences,
            color_environment,
            asset_library,
            proxy,
        }
    }

    /// Process-local identity of the exact open Authoring Session.
    pub(crate) const fn session_id(&self) -> AuthoringSessionId {
        self.session_id
    }

    /// Current Project author generation.
    pub(crate) const fn author_generation(&self) -> u64 {
        self.author_generation
    }

    /// Renderer binding identity for this validated immutable author snapshot.
    ///
    /// Preview rotates its Program cache whenever `session_id` changes, so the
    /// monotonic generation is unique within the active cache scope.
    pub(crate) const fn visual_author_snapshot_identity(
        &self,
    ) -> PreparedVisualAuthorSnapshotIdentity {
        PreparedVisualAuthorSnapshotIdentity::new(self.author_generation)
    }

    /// Canonical active Sequence.
    pub(crate) fn active_sequence(&self) -> Option<&Sequence> {
        self.sequences.active()
    }

    /// Complete canonical Sequence graph supplied to renderer closure preparation.
    pub(crate) fn sequences(&self) -> &[Sequence] {
        self.sequences.sequences.as_slice()
    }

    /// Project-owned color engine shared by every Sequence.
    pub(crate) const fn color_environment(&self) -> &ProjectColorEnvironment {
        self.color_environment
    }

    /// Exact open Project Asset Library.
    pub(crate) const fn asset_library(&self) -> &AssetLibrary {
        self.asset_library
    }

    /// Captured Project proxy-selection policy.
    pub(crate) const fn proxy(&self) -> &PreviewProxySelectionSnapshot<'a> {
        &self.proxy
    }

    /// Last content frame in the active Sequence.
    pub(crate) fn last_content_frame(&self) -> Option<i64> {
        let sequence = self.active_sequence()?;
        let end = sequence.total_duration().ok()?;
        end.to_frame_position(sequence.settings.frame_rate, FrameRounding::Nearest)
            .ok()
            .map(|position| position.frame.saturating_sub(1).max(0))
    }
}

/// One current Frame Demand captured together with its Adapter-domain deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewFrameDemandSnapshot {
    demand: FrameDemand,
    adapter_deadline: Option<Instant>,
}

impl PreviewFrameDemandSnapshot {
    /// Bind one authoritative demand to the deadline projected at capture time.
    pub(crate) const fn new(demand: FrameDemand, adapter_deadline: Option<Instant>) -> Self {
        Self { demand, adapter_deadline }
    }

    /// Opaque identity carried by Preview worker work.
    pub(crate) const fn identity(self) -> FrameDemandIdentity {
        self.demand.identity()
    }

    /// Adapter-domain deadline lowered exactly once for this turn.
    pub(crate) const fn adapter_deadline(self) -> Option<Instant> {
        self.adapter_deadline
    }

    /// Create presentation authority without rereading Playback state.
    pub(crate) const fn presentation_ticket(
        self,
        quality: FramePresentationQuality,
    ) -> FramePresentationTicket {
        FramePresentationTicket::for_demand(self.demand, quality)
    }
}

/// Coherent Playback facts sampled once for one Preview execution turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewTransportSnapshot {
    state: TransportState,
    position: FramePosition,
    epoch: PlaybackEpoch,
    runtime_scale: PreviewResolutionScale,
    seek_source: TimelineSeekSource,
    demand: Option<PreviewFrameDemandSnapshot>,
}

impl PreviewTransportSnapshot {
    /// Capture the exact transport facts consumed by Preview.
    pub(crate) const fn new(
        state: TransportState,
        position: FramePosition,
        epoch: PlaybackEpoch,
        runtime_scale: PreviewResolutionScale,
        seek_source: TimelineSeekSource,
        demand: Option<PreviewFrameDemandSnapshot>,
    ) -> Self {
        Self {
            state,
            position,
            epoch,
            runtime_scale,
            seek_source,
            demand,
        }
    }

    /// Exact current frame sampled from the Playback Engine.
    pub(crate) const fn current_frame(self) -> i64 {
        self.position.frame
    }

    /// Whether the Playback decoder family is authoritative.
    pub(crate) const fn is_playing(self) -> bool {
        matches!(
            self.state,
            TransportState::Priming | TransportState::Playing | TransportState::Recovering
        )
    }

    /// Whether startup preroll currently holds the Clock Master.
    pub(crate) const fn is_priming(self) -> bool {
        matches!(self.state, TransportState::Priming)
    }

    /// Playback Session identity.
    pub(crate) const fn epoch(self) -> PlaybackEpoch {
        self.epoch
    }

    /// Runtime-only resolution scale after Playback and resource policy merge.
    pub(crate) const fn runtime_scale(self) -> PreviewResolutionScale {
        self.runtime_scale
    }

    /// Most recent user seek interaction class.
    pub(crate) const fn seek_source(self) -> TimelineSeekSource {
        self.seek_source
    }

    /// Minimal transport identity consumed by Preview execution.
    pub(crate) const fn intent(self) -> PreviewTransportIntent {
        PreviewTransportIntent::new(self.is_playing(), self.epoch)
    }

    /// Current Frame Demand, if any.
    pub(crate) const fn demand(self) -> Option<PreviewFrameDemandSnapshot> {
        self.demand
    }
}

/// Borrowed immutable input for one production Preview execution turn.
pub(crate) struct PreviewExecutionSnapshot<'a> {
    authoring: Option<PreviewAuthoringSnapshot<'a>>,
    transport: PreviewTransportSnapshot,
    viewer_display: &'a DisplayManagementPolicy,
}

impl<'a> PreviewExecutionSnapshot<'a> {
    /// Capture the minimal authoring, transport, and Viewer policy facts.
    pub(crate) const fn new(
        authoring: Option<PreviewAuthoringSnapshot<'a>>,
        transport: PreviewTransportSnapshot,
        viewer_display: &'a DisplayManagementPolicy,
    ) -> Self {
        Self { authoring, transport, viewer_display }
    }

    /// Open Project authoring view, if a Project is active.
    pub(crate) const fn authoring(&self) -> Option<&PreviewAuthoringSnapshot<'a>> {
        self.authoring.as_ref()
    }

    /// Process-local open-session identity used to scope prepared execution.
    pub(crate) fn authoring_session_id(&self) -> Option<AuthoringSessionId> {
        self.authoring().map(PreviewAuthoringSnapshot::session_id)
    }

    /// Coherently sampled Playback facts.
    pub(crate) const fn transport(&self) -> PreviewTransportSnapshot {
        self.transport
    }

    /// Machine-local Viewer display policy.
    pub(crate) const fn viewer_display(&self) -> &DisplayManagementPolicy {
        self.viewer_display
    }

    /// Presentation authority derived from the already captured Frame Demand.
    pub(crate) fn presentation_ticket(
        &self,
        quality: FramePresentationQuality,
    ) -> Option<FramePresentationTicket> {
        self.transport.demand().map(|demand| demand.presentation_ticket(quality))
    }
}

/// Complete input for a frame-producing Preview execution call.
pub(crate) struct PreviewFrameExecutionRequest<'a> {
    snapshot: PreviewExecutionSnapshot<'a>,
    proxy_demands: &'a dyn PreviewProxyDemandSink,
}

impl<'a> PreviewFrameExecutionRequest<'a> {
    /// Bind an immutable execution snapshot to the only allowed external command seam.
    pub(crate) const fn new(
        snapshot: PreviewExecutionSnapshot<'a>,
        proxy_demands: &'a dyn PreviewProxyDemandSink,
    ) -> Self {
        Self { snapshot, proxy_demands }
    }

    /// Immutable execution facts.
    pub(crate) const fn snapshot(&self) -> &PreviewExecutionSnapshot<'a> {
        &self.snapshot
    }

    /// Proxy demand command seam.
    pub(crate) const fn proxy_demands(&self) -> &dyn PreviewProxyDemandSink {
        self.proxy_demands
    }
}

/// Narrow preroll input captured after terminal deliveries have been applied.
pub(crate) struct PreviewVideoPrerollRequest<'a> {
    snapshot: PreviewExecutionSnapshot<'a>,
    demand: Option<FrameDemandIdentity>,
    proxy_demands: &'a dyn PreviewProxyDemandSink,
}

impl<'a> PreviewVideoPrerollRequest<'a> {
    /// Create a preroll observation from a freshly captured execution snapshot.
    pub(crate) const fn new(
        snapshot: PreviewExecutionSnapshot<'a>,
        demand: Option<FrameDemandIdentity>,
        proxy_demands: &'a dyn PreviewProxyDemandSink,
    ) -> Self {
        Self { snapshot, demand, proxy_demands }
    }

    /// Immutable facts available to preroll observation.
    pub(crate) const fn snapshot(&self) -> &PreviewExecutionSnapshot<'a> {
        &self.snapshot
    }

    /// Exact current demand whose future media window is being inspected.
    pub(crate) const fn demand(&self) -> Option<FrameDemandIdentity> {
        self.demand
    }

    /// Proxy demand command seam retained separately from immutable facts.
    pub(crate) const fn proxy_demands(&self) -> &dyn PreviewProxyDemandSink {
        self.proxy_demands
    }
}
