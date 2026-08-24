//! Immutable selected-range dependency capture.
//!
//! The App supplies an author snapshot and external Asset Library Adapter.
//! This module owns the production interpretation of which Sequence,
//! Transition, and media identities can contribute to one export selection.

use crate::{
    ExportMediaDependency, ResolvedTimelineExportRange, TimelineExportRange,
    TimelineExportRangeError,
};
use mondrian_audio::{
    compile_audio_dependency_closure, AudioDependencyClosure, AudioDependencyError,
    AudioProgramExecutionDemand, CompiledAudioProgram,
};
use mondrian_core::{
    AssetId, AudioSourceComponentId, SequenceId, SequenceRevision, TimelineTime, VideoTransitionId,
};
use mondrian_effects::effect_registry_revision;
use mondrian_renderer::{
    prepare_visual_range_closure, BasicTitleFontQuery, BasicTitleRasterError,
    PreparedBasicTitleFontSet, PreparedVisualNestedRange, PreparedVisualProgram,
    PreparedVisualProgramCache, PreparedVisualProgramError, PreparedVisualRangeClosure,
    PreparedVisualRangeClosureError,
};
use mondrian_timeline::{
    validate_selected_video_transition_source_handles, PictureSourceExtent, PictureSourceRef,
    Sequence, VideoTransitionSourceHandleValidationError,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

const MAX_STABLE_VISUAL_CAPTURE_ATTEMPTS: usize = 32;

/// Non-persistent immutable visual execution closure captured for one export.
///
/// Every Program was used to derive the selected media, Transition, and nested
/// Sequence reachability in the same stable Effect-definition registry
/// revision. Export execution must consume these exact Programs; a later live
/// registry revision is not allowed to reinterpret the admitted snapshot.
#[derive(Debug, Clone)]
pub struct PreparedTimelineVisualSnapshot {
    closure: PreparedVisualRangeClosure,
    title_fonts: Option<PreparedBasicTitleFontSet>,
    retained_bytes: usize,
    range: ResolvedTimelineExportRange,
}

impl PreparedTimelineVisualSnapshot {
    /// Exact Effect-definition registry revision shared by every Program.
    pub const fn effect_registry_revision(&self) -> u64 {
        self.closure.effect_registry_revision()
    }

    /// Number of selected root/nested visual Programs.
    pub fn program_count(&self) -> usize {
        self.closure.programs().len()
    }

    /// Conservative aggregate logical bytes retained by the frozen Programs.
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    /// Return the exact immutable Program for one frozen Sequence revision.
    pub fn program(
        &self,
        sequence_id: SequenceId,
        sequence_revision: SequenceRevision,
    ) -> Option<Arc<PreparedVisualProgram>> {
        self.closure
            .program(sequence_id)
            .filter(|program| program.sequence_revision() == sequence_revision)
            .cloned()
    }

    /// Exact frozen Program for one selected Sequence identity.
    ///
    /// This crate-private lookup is used only after queue admission has
    /// validated the complete closure. Materializers consume the Program's
    /// typed static contract rather than reopening the authoring Sequence.
    pub(crate) fn program_by_id(
        &self,
        sequence_id: SequenceId,
    ) -> Option<Arc<PreparedVisualProgram>> {
        self.closure.program(sequence_id).cloned()
    }

    /// Root and nested Sequence identities selected by visual reachability.
    pub fn sequence_ids(&self) -> &BTreeSet<SequenceId> {
        self.closure.sequence_ids()
    }

    /// File-backed picture identities selected by visual reachability.
    pub fn media_asset_ids(&self) -> &BTreeSet<AssetId> {
        self.closure.media_asset_ids()
    }

    /// Selected Transition identities grouped by their owner Sequence.
    pub fn transition_ids(&self) -> &BTreeMap<SequenceId, BTreeSet<VideoTransitionId>> {
        self.closure.transition_ids()
    }

    /// Static Basic Title font queries selected by visual reachability.
    pub fn basic_title_font_queries(&self) -> &BTreeSet<BasicTitleFontQuery> {
        self.closure.basic_title_font_queries()
    }

    /// Exact byte-frozen title-font closure installed at queue admission.
    pub fn title_fonts(&self) -> Option<&PreparedBasicTitleFontSet> {
        self.title_fonts.as_ref()
    }

    /// Install the exact byte-frozen font closure for this visual selection.
    pub(crate) fn install_title_fonts(
        &mut self,
        title_fonts: PreparedBasicTitleFontSet,
    ) -> Result<(), TimelineExportDependencyError> {
        let queries = self.basic_title_font_queries();
        if title_fonts.binding_count() != queries.len()
            || queries.iter().any(|query| !title_fonts.contains_query(query))
        {
            return Err(TimelineExportDependencyError::TitleFontClosureEvidenceMismatch);
        }
        self.title_fonts = Some(title_fonts);
        Ok(())
    }

    /// Freeze every selected Basic Title font into this execution attempt.
    pub(crate) fn freeze_title_fonts(
        &mut self,
        max_retained_bytes: usize,
    ) -> Result<(), TimelineExportDependencyError> {
        if self.title_fonts.is_some() {
            return Ok(());
        }
        let title_fonts = PreparedBasicTitleFontSet::prepare(
            self.basic_title_font_queries().iter().cloned(),
            max_retained_bytes,
        )
        .map_err(TimelineExportDependencyError::TitleFontPreparation)?;
        self.install_title_fonts(title_fonts)
    }

    /// Exact frame selection whose reachability produced this closure.
    pub const fn range(&self) -> ResolvedTimelineExportRange {
        self.range
    }
}

/// Exact selected-range audio semantic evidence frozen for one export.
///
/// The retained root Program is the pure author-to-semantic result used for
/// admission, execution-demand selection, and root Runtime preparation. Audio
/// compilation does not consult processor registries, devices, files, or media
/// adapters.
#[derive(Debug, Clone)]
pub struct PreparedTimelineAudioSnapshot {
    closure: AudioDependencyClosure,
    root_program: Arc<CompiledAudioProgram>,
    sequence_ids: BTreeSet<SequenceId>,
    media_components: BTreeMap<AssetId, BTreeSet<AudioSourceComponentId>>,
    execution_demand: AudioProgramExecutionDemand,
    range: ResolvedTimelineExportRange,
}

impl PreparedTimelineAudioSnapshot {
    /// Exact immutable root semantic Program selected at admission.
    pub fn root_program(&self) -> &Arc<CompiledAudioProgram> {
        &self.root_program
    }

    /// Exact root/nested occurrence Programs and dependency evidence.
    pub fn closure(&self) -> &AudioDependencyClosure {
        &self.closure
    }

    /// Conservative proof governing whether PCM execution may be omitted.
    pub const fn execution_demand(&self) -> AudioProgramExecutionDemand {
        self.execution_demand
    }

    /// Root and nested Sequence identities selected by audio reachability.
    pub fn sequence_ids(&self) -> &BTreeSet<SequenceId> {
        &self.sequence_ids
    }

    /// Exact file-backed audio Component closure.
    pub fn media_components(&self) -> &BTreeMap<AssetId, BTreeSet<AudioSourceComponentId>> {
        &self.media_components
    }

    /// Exact public root range used for semantic selection.
    pub const fn range(&self) -> ResolvedTimelineExportRange {
        self.range
    }
}

/// Complete non-persistent execution attachment for one selected export.
#[derive(Debug, Clone)]
pub struct PreparedTimelineExecutionSnapshot {
    visual: PreparedTimelineVisualSnapshot,
    audio: Option<PreparedTimelineAudioSnapshot>,
}

impl PreparedTimelineExecutionSnapshot {
    /// Exact recursive visual Program and dependency closure.
    pub const fn visual(&self) -> &PreparedTimelineVisualSnapshot {
        &self.visual
    }

    /// Exact selected audio Program evidence when audio delivery is enabled.
    pub const fn audio(&self) -> Option<&PreparedTimelineAudioSnapshot> {
        self.audio.as_ref()
    }

    pub(crate) fn visual_mut(&mut self) -> &mut PreparedTimelineVisualSnapshot {
        &mut self.visual
    }
}

/// Immutable dependency demand selected before external media is resolved.
///
/// This is deliberately smaller than [`crate::TimelineExportSnapshot`]: it
/// contains selected identities plus non-persistent semantic Programs, but no
/// physical media records. The App's Asset Library Adapter resolves those
/// identities once into frozen physical dependencies.
#[derive(Debug, Clone)]
pub struct PreparedTimelineExportDependencies {
    sequence_ids: BTreeSet<SequenceId>,
    media_components: BTreeMap<AssetId, BTreeSet<AudioSourceComponentId>>,
    transition_ids: BTreeMap<SequenceId, BTreeSet<VideoTransitionId>>,
    range: ResolvedTimelineExportRange,
    execution_snapshot: PreparedTimelineExecutionSnapshot,
}

impl PreparedTimelineExportDependencies {
    /// Root and nested Sequence identities reachable from the selection.
    pub fn sequence_ids(&self) -> &BTreeSet<SequenceId> {
        &self.sequence_ids
    }

    /// File-backed media identities and exact audio Components to freeze.
    ///
    /// A visual-only Asset has an empty Component set.
    pub fn media_components(&self) -> &BTreeMap<AssetId, BTreeSet<AudioSourceComponentId>> {
        &self.media_components
    }

    /// Selected visual Transition source-handle checks grouped by owner Sequence.
    pub fn transition_ids(&self) -> &BTreeMap<SequenceId, BTreeSet<VideoTransitionId>> {
        &self.transition_ids
    }

    /// Exact root frame selection shared with execution.
    pub fn range(&self) -> ResolvedTimelineExportRange {
        self.range
    }

    /// Exact non-persistent visual/audio execution attachment.
    pub fn execution_snapshot(&self) -> &PreparedTimelineExecutionSnapshot {
        &self.execution_snapshot
    }
}

/// Failure while preparing one selected-range export dependency closure.
#[derive(Debug, thiserror::Error)]
pub enum TimelineExportDependencyError {
    /// Authored range could not be lowered onto the root frame grid.
    #[error(transparent)]
    Range(#[from] TimelineExportRangeError),
    /// Selected Audio Program dependency lowering failed.
    #[error(transparent)]
    Audio(#[from] AudioDependencyError),
    /// Candidate input contains duplicate immutable Sequence identities.
    #[error("export dependency candidates contain duplicate Sequence {0}")]
    DuplicateSequence(SequenceId),
    /// A selected nested visual placement references no candidate Sequence.
    #[error("export dependency closure is missing nested Sequence {0}")]
    MissingNestedSequence(SequenceId),
    /// Selected visual placements form a recursive Sequence graph.
    #[error("export dependency closure contains a nested Sequence cycle at {0}")]
    NestedCycle(SequenceId),
    /// Selected visual closure exceeds the shared nesting contract.
    #[error("export dependency closure exceeds nested depth {maximum} at Sequence {sequence_id}")]
    NestedDepthExceeded {
        /// Sequence that crossed the shared limit.
        sequence_id: SequenceId,
        /// Shared maximum nesting depth.
        maximum: usize,
    },
    /// Prepared visual scheduling or exact range projection failed.
    #[error("failed to prepare visual dependencies for Sequence {sequence_id}: {reason}")]
    Visual {
        /// Sequence whose immutable program failed.
        sequence_id: SequenceId,
        /// Typed renderer failure rendered for the App status seam.
        reason: String,
    },
    /// One selected Sequence violates the shared local author contract.
    #[error("selected Sequence {sequence_id} has invalid author state: {reason}")]
    AuthorContract {
        /// Invalid selected Sequence.
        sequence_id: SequenceId,
        /// Shared Timeline validation diagnostic.
        reason: String,
    },
    /// Canonical renderer range-closure preparation or validation failed.
    #[error(transparent)]
    VisualClosure(PreparedVisualRangeClosureError),
    /// Effect definitions changed while one candidate visual closure was built.
    #[error("Effect registry changed during export visual capture ({before} -> {after})")]
    EffectRegistryChanged {
        /// Revision observed before the conflicting preparation.
        before: u64,
        /// Revision observed after the conflicting preparation.
        after: u64,
    },
    /// No bounded retry observed one stable Effect-definition registry revision.
    #[error(
        "Effect registry did not stabilize while capturing export visual dependencies after {attempts} attempts ({before} -> {after})"
    )]
    EffectRegistryUnstable {
        /// Last revision observed before a conflicting attempt.
        before: u64,
        /// Last revision observed after a conflicting attempt.
        after: u64,
        /// Bounded number of attempted atomic captures.
        attempts: usize,
    },
    /// A frozen visual closure does not contain one selected Sequence revision.
    #[error(
        "frozen export visual closure is missing Sequence {sequence_id} revision {sequence_revision:?}"
    )]
    MissingPreparedVisualProgram {
        /// Selected Sequence identity.
        sequence_id: SequenceId,
        /// Exact immutable author revision.
        sequence_revision: SequenceRevision,
    },
    /// A frozen visual closure contains Programs outside its selected closure.
    #[error(
        "frozen export visual closure contains {actual} Programs for {expected} selected Sequences"
    )]
    UnexpectedPreparedVisualPrograms {
        /// Selected visual Sequence count.
        expected: usize,
        /// Frozen Program count.
        actual: usize,
    },
    /// Frozen reachability requires a media identity absent from the snapshot.
    #[error("frozen export dependency closure is missing media Asset {0}")]
    MissingMedia(AssetId),
    /// Frozen audio reachability requires a Component binding absent from media.
    #[error("frozen export dependency closure is missing audio Component {component_id} for Asset {asset_id}")]
    MissingAudioComponent {
        /// Required file-backed media identity.
        asset_id: AssetId,
        /// Required physical audio Component identity.
        component_id: AudioSourceComponentId,
    },
    /// The execution attachment was captured for another selected frame range.
    #[error("frozen export visual closure range does not match the admitted export range")]
    VisualRangeMismatch,
    /// Captured closure metadata does not match reachability from its Programs.
    #[error("frozen export visual closure evidence mismatch: {detail}")]
    VisualClosureEvidenceMismatch {
        /// Exact violated closure invariant.
        detail: String,
    },
    /// Snapshot author payload is not the exact selected Sequence closure.
    #[error("frozen export Sequence closure evidence mismatch")]
    SequenceClosureEvidenceMismatch,
    /// Snapshot media payload is not the exact selected Asset/Component closure.
    #[error("frozen export media closure evidence mismatch")]
    MediaClosureEvidenceMismatch,
    /// Frozen title-font bindings are not the exact selected query closure.
    #[error("frozen Basic Title font closure evidence mismatch")]
    TitleFontClosureEvidenceMismatch,
    /// Selected font faces could not be byte-frozen for this execution attempt.
    #[error("failed to freeze Basic Title font dependencies: {0}")]
    TitleFontPreparation(#[source] BasicTitleRasterError),
    /// Audio delivery is enabled but the frozen attachment contains no audio snapshot.
    #[error("audio-enabled export has no exact frozen root Program evidence")]
    MissingPreparedAudioProgram,
    /// Frozen audio Program or closure evidence differs from pure recompilation.
    #[error("frozen audio Program closure evidence mismatch: {detail}")]
    AudioClosureEvidenceMismatch {
        /// Exact violated audio attachment invariant.
        detail: String,
    },
    /// A renderer-selected Transition exceeds one frozen picture source.
    #[error(transparent)]
    TransitionSourceHandles(#[from] VideoTransitionSourceHandleValidationError),
}

/// Prepare the sole selected-range dependency demand consumed by snapshot capture.
///
/// Visual reachability comes from renderer-prepared schedules. Audio
/// reachability comes from the selected public Program Output's compiled
/// contributions. This function never walks Tracks or Clips.
pub fn prepare_timeline_export_dependencies(
    root: &Sequence,
    sequences: &[Sequence],
    range: TimelineExportRange,
    include_audio: bool,
) -> Result<PreparedTimelineExportDependencies, TimelineExportDependencyError> {
    let resolved_range = range.resolve(root)?;
    let (first, last) =
        resolved_range
            .visual_bounds()?
            .ok_or_else(|| TimelineExportDependencyError::Visual {
                sequence_id: root.id,
                reason: "resolved export visual range is empty".to_owned(),
            })?;
    let root_range = PreparedVisualNestedRange::Bounded { first, last };
    let mut last_registry_change = None;
    let mut stable_visual = None;
    for _ in 0..MAX_STABLE_VISUAL_CAPTURE_ATTEMPTS {
        let mut programs = PreparedVisualProgramCache::new(sequences.len().saturating_add(1));
        let mut preparation_registry_change = None;
        let result = prepare_visual_range_closure(root, sequences, root_range, |sequence| {
            programs.prepare(sequence).map_err(|error| {
                if let PreparedVisualProgramError::EffectRegistryChanged { before, after, .. } =
                    &error
                {
                    preparation_registry_change = Some((*before, *after));
                }
                error.to_string()
            })
        });
        if let Some(change) = preparation_registry_change {
            last_registry_change = Some(change);
            continue;
        }
        let closure = match result {
            Ok(closure) => closure,
            Err(PreparedVisualRangeClosureError::EffectRegistryRevisionMismatch {
                expected,
                actual,
                ..
            }) => {
                last_registry_change = Some((expected, actual));
                continue;
            }
            Err(error) => return Err(map_visual_closure_error(error)),
        };
        let final_revision = effect_registry_revision();
        if final_revision != closure.effect_registry_revision() {
            last_registry_change = Some((closure.effect_registry_revision(), final_revision));
            continue;
        }
        let retained_bytes = closure.programs().fold(0usize, |total, (_, program)| {
            total.saturating_add(program.retained_bytes_estimate())
        });
        let title_fonts = closure
            .basic_title_font_queries()
            .is_empty()
            .then(PreparedBasicTitleFontSet::default);
        let dependencies = PreparedTimelineExportDependencies {
            sequence_ids: closure.sequence_ids().clone(),
            media_components: closure
                .media_asset_ids()
                .iter()
                .copied()
                .map(|asset_id| (asset_id, BTreeSet::new()))
                .collect(),
            transition_ids: closure.transition_ids().clone(),
            range: resolved_range,
            execution_snapshot: PreparedTimelineExecutionSnapshot {
                visual: PreparedTimelineVisualSnapshot {
                    closure,
                    title_fonts,
                    retained_bytes,
                    range: resolved_range,
                },
                audio: None,
            },
        };
        stable_visual = Some(dependencies);
        break;
    }
    let mut dependencies = stable_visual.ok_or_else(|| {
        let (before, after) = last_registry_change.unwrap_or_default();
        TimelineExportDependencyError::EffectRegistryUnstable {
            before,
            after,
            attempts: MAX_STABLE_VISUAL_CAPTURE_ATTEMPTS,
        }
    })?;

    if include_audio {
        let audio =
            compile_audio_dependency_closure(root, sequences, None, resolved_range.time_range()?)?;
        dependencies.sequence_ids.extend(audio.sequence_ids().iter().copied());
        for (asset_id, components) in audio.media_components() {
            dependencies
                .media_components
                .entry(*asset_id)
                .or_default()
                .extend(components.iter().copied());
        }
        let root_program = Arc::clone(audio.root_program());
        let audio_sequence_ids = audio.sequence_ids().clone();
        let audio_media_components = audio.media_components().clone();
        let execution_demand = audio.execution_demand();
        dependencies.execution_snapshot.audio = Some(PreparedTimelineAudioSnapshot {
            closure: audio,
            root_program,
            sequence_ids: audio_sequence_ids,
            media_components: audio_media_components,
            execution_demand,
            range: resolved_range,
        });
    }
    Ok(dependencies)
}

/// Validate one complete immutable execution snapshot at queue admission.
///
/// Visual evidence is checked solely against the captured Programs and closure
/// metadata; the live Effect registry is deliberately not sampled. Pure audio
/// semantic compilation is repeated only as an admission integrity check
/// against the complete frozen occurrence closure. Worker Runtime preparation
/// consumes the frozen Programs rather than compiling again.
pub fn validate_timeline_export_execution_snapshot(
    root: &Sequence,
    sequences: &[Sequence],
    color_environment: &mondrian_core::ProjectColorEnvironment,
    range: TimelineExportRange,
    include_audio: bool,
    execution: &PreparedTimelineExecutionSnapshot,
    media: &HashMap<AssetId, ExportMediaDependency>,
) -> Result<(), TimelineExportDependencyError> {
    for sequence in std::iter::once(root).chain(sequences) {
        sequence.validate_author_contract(color_environment).map_err(|error| {
            TimelineExportDependencyError::AuthorContract {
                sequence_id: sequence.id,
                reason: error.to_string(),
            }
        })?;
    }
    let visual = execution.visual();
    let resolved_range = range.resolve(root)?;
    if visual.range() != resolved_range {
        return Err(TimelineExportDependencyError::VisualRangeMismatch);
    }
    let (first, last) =
        resolved_range
            .visual_bounds()?
            .ok_or_else(|| TimelineExportDependencyError::Visual {
                sequence_id: root.id,
                reason: "resolved export visual range is empty".to_owned(),
            })?;
    let closure = prepare_visual_range_closure(
        root,
        sequences,
        PreparedVisualNestedRange::Bounded { first, last },
        |sequence| {
            visual.program(sequence.id, sequence.revision).ok_or_else(|| {
                format!(
                    "Sequence {} revision {:?} is absent from the frozen export visual snapshot",
                    sequence.id, sequence.revision
                )
            })
        },
    )
    .map_err(map_visual_closure_error)?;
    if closure.sequence_ids() != visual.sequence_ids() {
        return Err(
            TimelineExportDependencyError::VisualClosureEvidenceMismatch {
                detail: "selected Sequence identities differ from frozen Program reachability"
                    .to_owned(),
            },
        );
    }
    if closure.media_asset_ids() != visual.media_asset_ids() {
        return Err(
            TimelineExportDependencyError::VisualClosureEvidenceMismatch {
                detail: "selected media identities differ from frozen Program reachability"
                    .to_owned(),
            },
        );
    }
    if closure.transition_ids() != visual.transition_ids() {
        return Err(
            TimelineExportDependencyError::VisualClosureEvidenceMismatch {
                detail: "selected Transition identities differ from frozen Program reachability"
                    .to_owned(),
            },
        );
    }
    let title_fonts = visual
        .title_fonts()
        .ok_or(TimelineExportDependencyError::TitleFontClosureEvidenceMismatch)?;
    if title_fonts.binding_count() != closure.basic_title_font_queries().len()
        || closure
            .basic_title_font_queries()
            .iter()
            .any(|query| !title_fonts.contains_query(query))
    {
        return Err(TimelineExportDependencyError::TitleFontClosureEvidenceMismatch);
    }
    if visual.program_count() != closure.programs().len()
        || closure.effect_registry_revision() != visual.effect_registry_revision()
    {
        return Err(
            TimelineExportDependencyError::UnexpectedPreparedVisualPrograms {
                expected: closure.programs().len(),
                actual: visual.program_count(),
            },
        );
    }
    let mut required_sequence_ids = visual.sequence_ids().clone();
    let mut required_media_components = visual
        .media_asset_ids()
        .iter()
        .copied()
        .map(|asset_id| (asset_id, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    match (include_audio, execution.audio()) {
        (true, Some(frozen_audio)) => {
            if frozen_audio.range() != resolved_range {
                return Err(
                    TimelineExportDependencyError::AudioClosureEvidenceMismatch {
                        detail: "selected audio range differs from the admitted export range"
                            .to_owned(),
                    },
                );
            }
            let audio = compile_audio_dependency_closure(
                root,
                sequences,
                None,
                visual.range().time_range()?,
            )?;
            let root_program = audio.root_program();
            if &audio != frozen_audio.closure()
                || root_program.as_ref() != frozen_audio.root_program().as_ref()
                || audio.execution_demand() != frozen_audio.execution_demand()
                || audio.sequence_ids() != frozen_audio.sequence_ids()
                || audio.media_components() != frozen_audio.media_components()
            {
                return Err(TimelineExportDependencyError::AudioClosureEvidenceMismatch {
                    detail:
                        "pure semantic recompilation differs from the frozen root Program or reachability"
                            .to_owned(),
                });
            }
            required_sequence_ids.extend(frozen_audio.sequence_ids().iter().copied());
            for (asset_id, components) in frozen_audio.media_components() {
                required_media_components
                    .entry(*asset_id)
                    .or_default()
                    .extend(components.iter().copied());
            }
        }
        (true, None) => return Err(TimelineExportDependencyError::MissingPreparedAudioProgram),
        (false, Some(_)) => {
            return Err(
                TimelineExportDependencyError::AudioClosureEvidenceMismatch {
                    detail: "audio execution evidence is present for an audio-disabled export"
                        .to_owned(),
                },
            );
        }
        (false, None) => {}
    }
    let actual_sequence_ids = std::iter::once(root.id)
        .chain(sequences.iter().map(|sequence| sequence.id))
        .collect::<BTreeSet<_>>();
    if sequences.iter().any(|sequence| sequence.id == root.id)
        || actual_sequence_ids != required_sequence_ids
    {
        return Err(TimelineExportDependencyError::SequenceClosureEvidenceMismatch);
    }
    if media.len() != required_media_components.len() {
        return Err(TimelineExportDependencyError::MediaClosureEvidenceMismatch);
    }
    for (asset_id, components) in &required_media_components {
        let dependency = media
            .get(asset_id)
            .ok_or(TimelineExportDependencyError::MissingMedia(*asset_id))?;
        for component_id in components {
            if !dependency.audio_components.contains_key(component_id) {
                return Err(TimelineExportDependencyError::MissingAudioComponent {
                    asset_id: *asset_id,
                    component_id: *component_id,
                });
            }
        }
        if dependency.audio_components.len() != components.len() {
            return Err(TimelineExportDependencyError::MediaClosureEvidenceMismatch);
        }
    }
    for (owner_id, transition_ids) in visual.transition_ids() {
        let owner = if *owner_id == root.id {
            root
        } else {
            sequences
                .iter()
                .find(|sequence| sequence.id == *owner_id)
                .ok_or(TimelineExportDependencyError::SequenceClosureEvidenceMismatch)?
        };
        validate_selected_video_transition_source_handles(owner, transition_ids, |source| {
            match source {
                PictureSourceRef::MediaAsset(asset_id) => {
                    media.get(&asset_id).and_then(|dependency| dependency.picture_source_extent)
                }
                PictureSourceRef::NestedSequence(sequence_id) => {
                    let sequence = if sequence_id == root.id {
                        Some(root)
                    } else {
                        sequences.iter().find(|sequence| sequence.id == sequence_id)
                    }?;
                    let duration = sequence.total_duration().ok()?;
                    let range =
                        mondrian_core::TimelineTimeRange::new(TimelineTime::ZERO, duration).ok()?;
                    Some(PictureSourceExtent::TimelineRange(range))
                }
            }
        })?;
    }
    Ok(())
}

fn map_visual_closure_error(
    error: PreparedVisualRangeClosureError,
) -> TimelineExportDependencyError {
    match error {
        PreparedVisualRangeClosureError::DuplicateSequenceIdentity { sequence_id } => {
            TimelineExportDependencyError::DuplicateSequence(sequence_id)
        }
        PreparedVisualRangeClosureError::MissingNestedSequence { nested_sequence_id, .. } => {
            TimelineExportDependencyError::MissingNestedSequence(nested_sequence_id)
        }
        PreparedVisualRangeClosureError::NestedCycle { path } => {
            path.last().copied().map(TimelineExportDependencyError::NestedCycle).unwrap_or(
                TimelineExportDependencyError::VisualClosure(
                    PreparedVisualRangeClosureError::NestedCycle { path },
                ),
            )
        }
        PreparedVisualRangeClosureError::NestedDepthExceeded { sequence_id, maximum } => {
            TimelineExportDependencyError::NestedDepthExceeded { sequence_id, maximum }
        }
        PreparedVisualRangeClosureError::EffectRegistryRevisionMismatch {
            expected,
            actual,
            ..
        } => {
            TimelineExportDependencyError::EffectRegistryChanged { before: expected, after: actual }
        }
        error => TimelineExportDependencyError::VisualClosure(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{AssetId, AudioSourceComponentId, Rational, TimelineTimeRange};
    use mondrian_timeline::{clip::Clip, track::Track, VideoTransition};

    fn tt(frame: i64, time_base: Rational) -> TimelineTime {
        TimelineTime::new(
            frame.checked_mul(time_base.num).expect("test time"),
            time_base.den,
        )
        .expect("test TimelineTime")
    }

    #[test]
    fn selected_range_excludes_hidden_and_off_range_visual_assets() {
        let mut sequence = Sequence::new("selected visual dependencies");
        sequence.video_tracks.clear();
        let time_base = sequence.time_base();
        let selected = AssetId::new();
        let off_range = AssetId::new();
        let hidden = AssetId::new();
        let mut visible_track = Track::new_video("visible");
        visible_track
            .add_clip(Clip::new(selected, tt(0, time_base), tt(10, time_base)).expect("selected"))
            .expect("add selected");
        visible_track
            .add_clip(
                Clip::new(off_range, tt(20, time_base), tt(10, time_base)).expect("off-range"),
            )
            .expect("add off-range");
        let mut hidden_track = Track::new_video("hidden");
        hidden_track.is_visible = false;
        hidden_track
            .add_clip(Clip::new(hidden, tt(0, time_base), tt(10, time_base)).expect("hidden"))
            .expect("add hidden");
        sequence.video_tracks.extend([visible_track, hidden_track]);

        let dependencies = prepare_timeline_export_dependencies(
            &sequence,
            &[],
            TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 10 },
            false,
        )
        .expect("selected dependencies");
        assert!(dependencies.media_components().contains_key(&selected));
        assert!(!dependencies.media_components().contains_key(&off_range));
        assert!(!dependencies.media_components().contains_key(&hidden));
        let visual = dependencies.execution_snapshot().visual();
        assert!(visual.basic_title_font_queries().is_empty());
        assert_eq!(
            visual
                .title_fonts()
                .expect("empty font-query closure is exact at capture")
                .binding_count(),
            0
        );
    }

    #[test]
    fn selected_title_font_queries_remain_unresolved_until_the_font_adapter_freezes_bytes() {
        let mut sequence = Sequence::new("selected Basic Title font dependencies");
        let time_base = sequence.time_base();
        sequence.video_tracks[0]
            .add_clip(
                Clip::new_basic_title(
                    "selected title",
                    "Mondrian Capture Test Face",
                    tt(0, time_base),
                    tt(10, time_base),
                )
                .expect("Basic Title"),
            )
            .expect("add Basic Title");

        let dependencies = prepare_timeline_export_dependencies(
            &sequence,
            &[],
            TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 10 },
            false,
        )
        .expect("selected dependencies");
        let visual = dependencies.execution_snapshot().visual();

        assert_eq!(visual.basic_title_font_queries().len(), 1);
        assert!(
            visual.title_fonts().is_none(),
            "non-empty font queries require exact face bytes from the font Adapter"
        );
    }

    #[test]
    fn illegal_selected_author_state_cannot_become_execution_authority() {
        let sequence = Sequence::new("author contract admission");
        let range = TimelineExportRange::EntireSequence;
        let dependencies = prepare_timeline_export_dependencies(&sequence, &[], range, false)
            .expect("prepare valid candidate");
        let mut execution = dependencies.execution_snapshot().clone();
        execution
            .visual_mut()
            .install_title_fonts(PreparedBasicTitleFontSet::default())
            .expect("seal empty title-font closure");
        let mut invalid = sequence;
        invalid.video_tracks[0].height = f32::NAN;

        assert!(matches!(
            validate_timeline_export_execution_snapshot(
                &invalid,
                &[],
                &mondrian_core::ProjectColorEnvironment::default(),
                range,
                false,
                &execution,
                &HashMap::new(),
            ),
            Err(TimelineExportDependencyError::AuthorContract { .. })
        ));
    }

    #[test]
    fn selected_audio_output_adds_only_intersecting_component_binding() {
        let mut sequence = Sequence::new("selected audio dependencies");
        let time_base = sequence.time_base();
        let track_id = sequence.audio_tracks[0].id;
        let selected = AssetId::new();
        let off_range = AssetId::new();
        sequence
            .add_media_audio_clip(
                track_id,
                Clip::new(selected, tt(0, time_base), tt(10, time_base)).expect("selected"),
                AudioSourceComponentId::primary(),
            )
            .expect("selected audio");
        sequence
            .add_media_audio_clip(
                track_id,
                Clip::new(off_range, tt(20, time_base), tt(10, time_base)).expect("off-range"),
                AudioSourceComponentId::primary(),
            )
            .expect("off-range audio");

        let dependencies = prepare_timeline_export_dependencies(
            &sequence,
            &[],
            TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 10 },
            true,
        )
        .expect("selected dependencies");
        assert_eq!(
            dependencies.media_components().get(&selected),
            Some(&BTreeSet::from([AudioSourceComponentId::primary()]))
        );
        assert!(!dependencies.media_components().contains_key(&off_range));
    }

    #[test]
    fn frozen_audio_closure_binds_nested_processor_semantics_not_only_ids() {
        let child = Sequence::new("nested audio semantics");
        let child_output = child.audio_program.outputs[0].id;
        let mut root = Sequence::new("root audio semantics");
        let time_base = root.time_base();
        let track_id = root.audio_tracks[0].id;
        root.add_nested_audio_clip(
            track_id,
            Clip::new_nested_sequence(
                child.id,
                tt(0, time_base),
                tt(10, time_base),
                Some("nested".to_owned()),
            )
            .expect("nested Clip"),
            child_output,
        )
        .expect("nested audio edit");
        let range = TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 10 };
        let dependencies =
            prepare_timeline_export_dependencies(&root, std::slice::from_ref(&child), range, true)
                .expect("prepare exact nested audio closure");
        let mut execution = dependencies.execution_snapshot().clone();
        execution
            .visual_mut()
            .install_title_fonts(PreparedBasicTitleFontSet::default())
            .expect("seal empty title-font closure");

        let mut changed_child = child;
        changed_child.audio_program.outputs[0].strip.pre_fader.processors.push(
            mondrian_timeline::AudioProcessorInstance {
                id: mondrian_core::AudioProcessorInstanceId::new(),
                definition: mondrian_timeline::AudioProcessorDefinitionRef::Clap {
                    plugin_id: "test.mondrian.changed-after-capture".to_owned(),
                    schema_version: 1,
                },
                bypassed: false,
                parameters: Default::default(),
                opaque_state: None,
            },
        );

        assert!(matches!(
            validate_timeline_export_execution_snapshot(
                &root,
                &[changed_child],
                &mondrian_core::ProjectColorEnvironment::default(),
                range,
                true,
                &execution,
                &HashMap::new(),
            ),
            Err(TimelineExportDependencyError::AudioClosureEvidenceMismatch { .. })
        ));
    }

    #[test]
    fn transition_handle_demands_follow_the_prepared_selected_interval() {
        let mut sequence = Sequence::new("selected Transition handles");
        sequence.video_tracks.clear();
        let time_base = sequence.time_base();
        let mut track = Track::new_video("V1");
        let left = Clip::new(AssetId::new(), tt(0, time_base), tt(10, time_base)).expect("left");
        let right = Clip::new(AssetId::new(), tt(10, time_base), tt(10, time_base)).expect("right");
        let (left_id, right_id) = (left.id, right.id);
        track.add_clip(left).expect("add left");
        track.add_clip(right).expect("add right");
        sequence.video_tracks.push(track);
        let transition = VideoTransition::cross_dissolve(
            left_id,
            right_id,
            TimelineTimeRange::new(tt(8, time_base), tt(4, time_base)).expect("Transition range"),
        );
        let transition_id = transition.id;
        sequence.video_transitions.push(transition);

        let outside = prepare_timeline_export_dependencies(
            &sequence,
            &[],
            TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 5 },
            false,
        )
        .expect("outside dependencies");
        assert!(outside.transition_ids().is_empty());

        let selected = prepare_timeline_export_dependencies(
            &sequence,
            &[],
            TimelineExportRange::WorkArea { start_frame: 9, end_frame_exclusive: 10 },
            false,
        )
        .expect("selected dependencies");
        assert_eq!(
            selected.transition_ids().get(&sequence.id),
            Some(&BTreeSet::from([transition_id]))
        );
    }

    #[test]
    fn frozen_picture_extents_validate_only_the_selected_transition_closure() {
        let mut sequence = Sequence::new("frozen Transition picture extents");
        sequence.video_tracks.clear();
        let time_base = sequence.time_base();
        let left_asset = AssetId::new();
        let right_asset = AssetId::new();
        let mut track = Track::new_video("V1");
        let left = Clip::new(left_asset, tt(0, time_base), tt(10, time_base)).expect("left Clip");
        let right =
            Clip::new(right_asset, tt(10, time_base), tt(10, time_base)).expect("right Clip");
        let transition = VideoTransition::cross_dissolve(
            left.id,
            right.id,
            TimelineTimeRange::new(tt(8, time_base), tt(4, time_base)).expect("Transition range"),
        );
        track.add_clip(left).expect("add left");
        track.add_clip(right).expect("add right");
        sequence.video_tracks.push(track);
        sequence.video_transitions.push(transition);
        let range = TimelineExportRange::WorkArea { start_frame: 9, end_frame_exclusive: 10 };
        let dependencies = prepare_timeline_export_dependencies(&sequence, &[], range, false)
            .expect("prepare selected Transition closure");
        let mut execution = dependencies.execution_snapshot().clone();
        execution
            .visual_mut()
            .install_title_fonts(PreparedBasicTitleFontSet::default())
            .expect("seal empty title-font closure");
        let dependency = |asset_id, picture_source_extent| {
            let path = std::path::PathBuf::from(format!("{asset_id}.mov"));
            (
                asset_id,
                ExportMediaDependency {
                    source_fingerprint: mondrian_media::MediaFileFingerprint::capture(
                        path.as_path(),
                    ),
                    path,
                    video_stream_index: Some(0),
                    picture_source_extent: Some(picture_source_extent),
                    source_resolution: Some(mondrian_core::Resolution {
                        width: 1920,
                        height: 1080,
                    }),
                    picture: Some(Default::default()),
                    audio_components: HashMap::new(),
                    interpretation: mondrian_core::timeline_data::AssetMediaInterpretation::default(
                    ),
                    color_diagnostic: None,
                },
            )
        };
        let still_media = HashMap::from([
            dependency(left_asset, PictureSourceExtent::Still),
            dependency(right_asset, PictureSourceExtent::Still),
        ]);
        validate_timeline_export_execution_snapshot(
            &sequence,
            &[],
            &mondrian_core::ProjectColorEnvironment::default(),
            range,
            false,
            &execution,
            &still_media,
        )
        .expect("still-picture hold satisfies exact Transition demands");

        let short = PictureSourceExtent::TimelineRange(
            TimelineTimeRange::new(TimelineTime::ZERO, tt(10, time_base)).expect("moving extent"),
        );
        let moving_media = HashMap::from([
            dependency(left_asset, short),
            dependency(right_asset, short),
        ]);
        assert!(matches!(
            validate_timeline_export_execution_snapshot(
                &sequence,
                &[],
                &mondrian_core::ProjectColorEnvironment::default(),
                range,
                false,
                &execution,
                &moving_media,
            ),
            Err(TimelineExportDependencyError::TransitionSourceHandles(
                VideoTransitionSourceHandleValidationError::InsufficientSourceHandles { .. }
            ))
        ));
    }

    #[test]
    fn off_range_missing_nested_sequence_does_not_block_selected_capture() {
        let mut sequence = Sequence::new("off-range missing child");
        sequence.video_tracks.clear();
        let time_base = sequence.time_base();
        let mut track = Track::new_video("V1");
        track
            .add_clip(
                Clip::new_nested_sequence(
                    SequenceId::new(),
                    tt(20, time_base),
                    tt(10, time_base),
                    Some("missing".to_owned()),
                )
                .expect("nested Clip"),
            )
            .expect("add nested");
        sequence.video_tracks.push(track);

        prepare_timeline_export_dependencies(
            &sequence,
            &[],
            TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: 10 },
            false,
        )
        .expect("off-range missing child is not a selected dependency");
    }

    #[test]
    fn selected_missing_nested_sequence_fails_closed() {
        let mut sequence = Sequence::new("selected missing child");
        sequence.video_tracks.clear();
        let time_base = sequence.time_base();
        let missing = SequenceId::new();
        let mut track = Track::new_video("V1");
        track
            .add_clip(
                Clip::new_nested_sequence(
                    missing,
                    tt(0, time_base),
                    tt(10, time_base),
                    Some("missing".to_owned()),
                )
                .expect("nested Clip"),
            )
            .expect("add nested");
        sequence.video_tracks.push(track);

        assert!(matches!(
            prepare_timeline_export_dependencies(
                &sequence,
                &[],
                TimelineExportRange::WorkArea {
                    start_frame: 0,
                    end_frame_exclusive: 10,
                },
                false,
            ),
            Err(TimelineExportDependencyError::MissingNestedSequence(id)) if id == missing
        ));
    }

    #[test]
    fn resolved_range_preserves_half_open_audio_and_inclusive_visual_views() {
        let sequence = Sequence::new("range views");
        let resolved = TimelineExportRange::WorkArea { start_frame: 3, end_frame_exclusive: 7 }
            .resolve(&sequence)
            .expect("resolved");
        let time_base = sequence.time_base();
        assert_eq!(
            resolved.time_range().expect("time range"),
            TimelineTimeRange::new(tt(3, time_base), tt(4, time_base)).expect("expected")
        );
        assert_eq!(
            resolved.visual_bounds().expect("visual bounds"),
            Some((tt(3, time_base), tt(6, time_base)))
        );
    }
}
