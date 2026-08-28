//! Revision-bound visual execution preparation.
//!
//! [`PreparedVisualSchedule`] owns placement indexing. This Module adds the
//! resource-bound Effect programs required to lower those selected placements
//! without resolving definitions, parsing LUTs, or compiling static graph
//! topology on every frame. It does not own Preview/Export scheduling, media
//! decode, GPU residency, or nested-Sequence recursion.

use crate::basic_title::BasicTitleFontQuery;
use mondrian_core::timeline_data::{
    ClipContent, FlatActiveClip, FlatVisualItem, RenderPlanSource, TimelineClipEndpointContext,
    TimelineClipExecutionRef,
};
use mondrian_core::{
    AssetId, ClipId, FramePosition, FrameRounding, GradeDefinitionId, MondrianError, Rational,
    Resolution, Result, SequenceId, SequenceRevision, TimelineTime, VideoTransitionId,
};
use mondrian_effects::{
    effect_registry_revision, prepare_temporal_frame_execution, CompiledEffectGraph,
    EffectExecutionEnvelope, EffectExecutionSession, EffectTemporalExecutionRequest,
    EffectTemporalSpan, LutPreparationCache, LutPreparationCacheConfig, PreparedEffectProgram,
    PreparedEffectTemporalExecution, PreparedGradeGraph,
};
use mondrian_timeline::{Clip, PreparedVisualSchedule, Sequence, VideoTransitionType};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// Default number of immutable Sequence execution programs retained by one
/// Preview or Export consumer.
pub const DEFAULT_PREPARED_VISUAL_PROGRAM_CACHE_CAPACITY: usize = 64;
/// Default conservative logical bytes retained by one visual-program cache.
pub const DEFAULT_PREPARED_VISUAL_PROGRAM_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_VISUAL_DEFINITION_BIND_RETRIES: usize = 32;

type PreparedClipGradeSegments = (Vec<PreparedGradeGraph>, Vec<PreparedGradeGraph>);
type PreparedGradeBlocker = (Arc<str>, PreparedVisualBlockerRetry);

fn prepared_clip_grade_segments(
    sequence: &Sequence,
    clip: &Clip,
    grades: &HashMap<GradeDefinitionId, PreparedSharedGrade>,
) -> std::result::Result<PreparedClipGradeSegments, PreparedGradeBlocker> {
    let group = clip
        .grade_group
        .and_then(|id| sequence.grade_groups.iter().find(|group| group.id == id));
    let before_ids = group.into_iter().filter_map(|group| group.pre_clip_grade);
    let after_ids = clip
        .grade
        .into_iter()
        .chain(group.into_iter().filter_map(|group| group.post_clip_grade));
    let resolve = |id| match grades.get(&id) {
        Some(PreparedSharedGrade::Ready(grade)) => Ok(grade.clone()),
        Some(PreparedSharedGrade::Blocked { reason, retry }) => Err((Arc::clone(reason), *retry)),
        None => Err((
            Arc::from(format!("missing grade definition {id}")),
            PreparedVisualBlockerRetry::AuthorOrDefinitionChange,
        )),
    };
    Ok((
        before_ids.map(resolve).collect::<std::result::Result<Vec<_>, _>>()?,
        after_ids.map(resolve).collect::<std::result::Result<Vec<_>, _>>()?,
    ))
}

fn hierarchical_clip_author_fingerprint(
    sequence: &Sequence,
    clip: &Clip,
    working_color_space: mondrian_core::WorkingColorSpace,
) -> std::result::Result<[u8; 32], String> {
    let group = clip
        .grade_group
        .and_then(|id| sequence.grade_groups.iter().find(|group| group.id == id));
    let definition_ids = group
        .into_iter()
        .filter_map(|group| group.pre_clip_grade)
        .chain(clip.grade)
        .chain(group.into_iter().filter_map(|group| group.post_clip_grade))
        .collect::<Vec<_>>();
    let definitions = definition_ids
        .iter()
        .filter_map(|id| sequence.grade_definition(*id))
        .collect::<Vec<_>>();
    let canonical = serde_json::to_vec(&(
        &clip.effects,
        &clip.masks,
        clip.grade,
        clip.grade_group,
        group,
        definitions,
        working_color_space,
    ))
    .map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.hierarchical-clip-author-fingerprint.v1");
    hasher.update((canonical.len() as u64).to_le_bytes());
    hasher.update(canonical);
    Ok(hasher.finalize().into())
}

fn visual_frame_seed(sequence_time: TimelineTime, rate: Rational) -> i64 {
    if let Ok(position) = sequence_time.to_frame_position(rate, FrameRounding::Nearest)
        && TimelineTime::from_frame_position(position).ok() == Some(sequence_time)
    {
        return position.frame;
    }
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.visual-off-grid-frame-seed.v1");
    hasher.update(sequence_time.numerator().to_le_bytes());
    hasher.update(sequence_time.denominator().to_le_bytes());
    hasher.update(rate.num.to_le_bytes());
    hasher.update(rate.den.to_le_bytes());
    let digest = hasher.finalize();
    i64::from_le_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PreparedVisualProgramKey {
    sequence_id: SequenceId,
    sequence_revision: SequenceRevision,
    effect_registry_revision: u64,
    visual_author_fingerprint: [u8; 32],
}

/// Process-local identity of one validated, immutable Project author snapshot.
///
/// The App constructs this value from its monotonic author generation after
/// rotating the owning cache at every Authoring Session change. Reusing an
/// identity for mutated author state violates the authoring-session contract;
/// raw or deserialized Sequences must use the checked visual-program
/// Interfaces instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PreparedVisualAuthorSnapshotIdentity {
    author_generation: u64,
}

impl PreparedVisualAuthorSnapshotIdentity {
    /// Bind one exact validated authoring generation.
    pub const fn new(author_generation: u64) -> Self {
        Self { author_generation }
    }

    /// Monotonic author generation captured with the snapshot.
    pub const fn author_generation(self) -> u64 {
        self.author_generation
    }
}

impl PreparedVisualProgramKey {
    fn for_sequence_with_author_fingerprint(
        sequence: &Sequence,
        visual_author_fingerprint: [u8; 32],
    ) -> Self {
        Self {
            sequence_id: sequence.id,
            sequence_revision: sequence.revision,
            effect_registry_revision: effect_registry_revision(),
            visual_author_fingerprint,
        }
    }
}

/// Failure to encode the versioned conservative visual-author identity.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Sequence {sequence_id} visual author fingerprint could not be encoded: {reason}")]
pub struct PreparedVisualAuthorFingerprintError {
    /// Sequence whose visual author projection could not be encoded.
    pub sequence_id: SequenceId,
    /// Canonical encoding diagnostic.
    pub reason: String,
}

/// Compute the versioned conservative identity of all Sequence author state
/// that can affect prepared picture execution.
///
/// The projection includes the exact video Track/Clip and Transition
/// containers plus frame geometry, evaluation rate, pixel interpretation,
/// color workflow, authored Preview raster scale, and Basic Title safe-area
/// settings. It deliberately
/// includes complete video Track records so a newly introduced visual field
/// cannot silently evade snapshot validation; changing that projection
/// requires a domain/version bump.
pub fn prepared_visual_author_fingerprint(
    sequence: &Sequence,
) -> std::result::Result<[u8; 32], PreparedVisualAuthorFingerprintError> {
    let canonical = serde_json::to_vec(&(
        sequence.settings.resolution,
        sequence.settings.frame_rate,
        sequence.settings.pixel_aspect_ratio,
        sequence.settings.field_order,
        &sequence.settings.color,
        sequence.settings.preview.resolution_scale,
        sequence.settings.title_safe_margin,
        &sequence.video_tracks,
        &sequence.video_transitions,
        &sequence.grade_definitions,
        &sequence.grade_groups,
        sequence.timeline_grade,
    ))
    .map_err(|error| PreparedVisualAuthorFingerprintError {
        sequence_id: sequence.id,
        reason: error.to_string(),
    })?;
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.prepared-visual-author-fingerprint.v3");
    hasher.update((canonical.len() as u64).to_le_bytes());
    hasher.update(canonical);
    Ok(hasher.finalize().into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ScopedPreparedVisualProgramKey {
    scope_generation: u64,
    program: PreparedVisualProgramKey,
}

impl ScopedPreparedVisualProgramKey {
    const fn new(scope_generation: u64, program: PreparedVisualProgramKey) -> Self {
        Self { scope_generation, program }
    }
}

#[derive(Debug, Clone)]
enum PreparedClipEffects {
    Ready {
        program: PreparedEffectProgram,
        author_fingerprint: [u8; 32],
    },
    Blocked {
        reason: Arc<str>,
        retry: PreparedVisualBlockerRetry,
    },
}

#[derive(Debug, Clone)]
enum PreparedSharedGrade {
    Ready(PreparedGradeGraph),
    Blocked {
        reason: Arc<str>,
        retry: PreparedVisualBlockerRetry,
    },
}

#[derive(Debug, Clone)]
enum PreparedTimelineGrade {
    None,
    Ready(PreparedEffectProgram),
    Blocked {
        reason: Arc<str>,
        retry: PreparedVisualBlockerRetry,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreparedVisualBlockerRetry {
    ExternalChange,
    AuthorOrDefinitionChange,
}

#[derive(Debug, Clone)]
enum PreparedVisualTransition {
    Ready,
    Blocked(Arc<str>),
}

/// One effect-preparation failure retained without blocking unrelated frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedVisualEffectBlocker {
    /// Clip whose visible execution cannot currently be prepared.
    pub clip_id: ClipId,
    /// Stable diagnostic suitable for Preview unavailability or Export
    /// preflight evidence.
    pub reason: Arc<str>,
}

/// One visual Transition preparation failure retained until its authored
/// interval is reachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedVisualTransitionBlocker {
    /// Transition whose definition cannot currently execute.
    pub transition_id: VideoTransitionId,
    /// Stable fail-closed diagnostic.
    pub reason: Arc<str>,
}

/// One nested Sequence sample proven reachable from an admitted visual frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PreparedVisualNestedDemand {
    /// Child Sequence selected by the nested Clip.
    pub sequence_id: SequenceId,
    /// Exact child-local time emitted by the canonical Clip source-time map.
    pub source_sample: mondrian_core::SourceSampleTarget,
}

/// Status-only reachability result for one prepared visual frame.
///
/// The result contains no pixels or Effect graph evaluation. Export uses it to
/// preflight exactly the requested root frames and their nested closure before
/// creating an encoder process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreparedVisualFrameReachability {
    /// Nested Sequence samples required by visible ordinary or Transition
    /// endpoint Clips at this frame.
    pub nested_demands: Vec<PreparedVisualNestedDemand>,
}

/// One child-Sequence dependency window retained by range-level preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreparedVisualNestedRange {
    /// Inclusive child-local time bounds conservatively reached by the parent
    /// placement and its finite temporal Effect extent.
    Bounded {
        /// Earliest reachable child-local coordinate.
        first: TimelineTime,
        /// Latest reachable child-local coordinate.
        last: TimelineTime,
    },
    /// An unbounded temporal Effect makes every child interval potentially
    /// reachable.
    WholeSequence,
}

/// One nested Sequence selected by immutable range-level visual preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PreparedVisualNestedRangeDemand {
    /// Child Sequence selected by the nested Clip.
    pub sequence_id: SequenceId,
    /// Conservative child-local dependency window.
    pub range: PreparedVisualNestedRange,
}

/// Immutable media dependency evidence for one inclusive Sequence window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreparedVisualRangeReachability {
    /// File-backed media assets that can contribute directly.
    pub media_asset_ids: Vec<AssetId>,
    /// Enabled visual Transitions whose endpoint execution intersects the window.
    ///
    /// External source-handle Adapters use this typed demand without repeating
    /// Track/Clip range interpretation.
    pub transition_ids: Vec<VideoTransitionId>,
    /// Static font queries selected by reachable Basic Title Clips and
    /// Transition endpoints.
    pub basic_title_font_queries: Vec<BasicTitleFontQuery>,
    /// Nested Sequence windows that can contribute transitively.
    pub nested_demands: Vec<PreparedVisualNestedRangeDemand>,
}

/// Static evidence produced while preparing one Sequence revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedVisualProgramDiagnostics {
    /// Sequence whose visual execution state was prepared.
    pub sequence_id: SequenceId,
    /// Exact conservative author revision.
    pub sequence_revision: SequenceRevision,
    /// Exact process-local Effect definition registry revision.
    pub effect_registry_revision: u64,
    /// Visible, enabled Clip occurrences with prepared Effect programs.
    pub prepared_clips: usize,
    /// Prepared Clip programs reused unchanged from the preceding author
    /// revision.
    pub reused_clips: usize,
    /// Visible, enabled Clip occurrences retained as fail-closed blockers.
    pub blocked_clips: usize,
    /// Visible, enabled visual Transitions whose definitions prepared.
    pub prepared_transitions: usize,
    /// Visible, enabled visual Transitions retained as fail-closed blockers.
    pub blocked_transitions: usize,
}

/// Immutable Sequence canvas facts required by frame materialization.
///
/// These values are frozen with the same author fingerprint and Definition
/// registry revision as the Program. Preview and Export materializers consume
/// this contract instead of reopening the authoring [`Sequence`] by identity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreparedVisualMaterializationContract {
    author_resolution: Resolution,
    authored_preview_resolution_scale: f32,
    title_safe_margin: f32,
}

impl PreparedVisualMaterializationContract {
    /// Sequence's authored logical raster.
    pub const fn author_resolution(self) -> Resolution {
        self.author_resolution
    }

    /// Sequence-authored Preview raster scale before runtime normalization.
    pub const fn authored_preview_resolution_scale(self) -> f32 {
        self.authored_preview_resolution_scale
    }

    /// Sequence-local Basic Title safe-area margin.
    pub const fn title_safe_margin(self) -> f32 {
        self.title_safe_margin
    }
}

/// Immutable visual execution program shared by Preview and Export.
///
/// Preparation failures are retained per Clip or Transition. Interactive
/// Preview therefore remains usable on unrelated timeline regions, while
/// Export composes [`Self::preflight_frame`] across its selected root range and
/// nested reachability closure before admitting encoder execution.
pub struct PreparedVisualProgram {
    key: PreparedVisualProgramKey,
    materialization: PreparedVisualMaterializationContract,
    schedule: Arc<PreparedVisualSchedule>,
    clip_effects: HashMap<ClipId, PreparedClipEffects>,
    timeline_grade: PreparedTimelineGrade,
    transitions: HashMap<VideoTransitionId, PreparedVisualTransition>,
    diagnostics: PreparedVisualProgramDiagnostics,
    retained_bytes_estimate: usize,
}

impl std::fmt::Debug for PreparedVisualProgram {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedVisualProgram")
            .field("key", &self.key)
            .field("materialization", &self.materialization)
            .field("clip_effect_count", &self.clip_effects.len())
            .field("transition_count", &self.transitions.len())
            .field("diagnostics", &self.diagnostics)
            .finish_non_exhaustive()
    }
}

impl PreparedVisualProgram {
    /// Prepare placement indexes and every visible, enabled Clip Effect program.
    ///
    /// A concurrent Effect-definition registry mutation fails atomically;
    /// preparation retries against the next exact registry revision before a
    /// persistent mutation storm is reported fail-closed.
    pub fn prepare(sequence: &Sequence) -> std::result::Result<Self, PreparedVisualProgramError> {
        let lut_cache = LutPreparationCache::uncached();
        let visual_author_fingerprint = prepared_visual_author_fingerprint(sequence)?;
        let mut retries = 0;
        loop {
            let key = PreparedVisualProgramKey::for_sequence_with_author_fingerprint(
                sequence,
                visual_author_fingerprint,
            );
            match Self::prepare_reusing_with_key(sequence, None, &lut_cache, key) {
                Err(PreparedVisualProgramError::EffectRegistryChanged { .. })
                    if retries + 1 < MAX_VISUAL_DEFINITION_BIND_RETRIES =>
                {
                    retries += 1;
                }
                result => return result,
            }
        }
    }

    fn prepare_reusing_with_key(
        sequence: &Sequence,
        previous: Option<&Self>,
        lut_cache: &LutPreparationCache,
        key: PreparedVisualProgramKey,
    ) -> std::result::Result<Self, PreparedVisualProgramError> {
        let schedule = Arc::new(PreparedVisualSchedule::compile(sequence).map_err(|error| {
            PreparedVisualProgramError::Schedule {
                sequence_id: sequence.id,
                reason: error.to_string(),
            }
        })?);
        let mut clip_effects = HashMap::new();
        let mut prepared_clips = 0;
        let mut reused_clips = 0;
        let mut blocked_clips = 0;
        let mut transitions = HashMap::new();
        let mut prepared_transitions = 0;
        let mut blocked_transitions = 0;
        let working_color_space = sequence.settings.color.working_color_space;
        let prepared_grades = sequence
            .grade_definitions
            .iter()
            .map(|definition| {
                let prepared = definition
                    .active()
                    .ok_or_else(|| "active grade version does not exist".to_owned())
                    .and_then(|version| {
                        PreparedGradeGraph::prepare_with_lut_cache(
                            &version.graph,
                            working_color_space,
                            lut_cache,
                        )
                        .map_err(|error| error.to_string())
                    });
                let prepared = match prepared {
                    Ok(grade) => PreparedSharedGrade::Ready(grade),
                    Err(reason) => PreparedSharedGrade::Blocked {
                        reason: Arc::from(reason),
                        retry: PreparedVisualBlockerRetry::AuthorOrDefinitionChange,
                    },
                };
                (definition.id, prepared)
            })
            .collect::<HashMap<_, _>>();
        let identity_author_fingerprint =
            clip_effect_author_fingerprint(&[], &[], working_color_space).map_err(|reason| {
                PreparedVisualProgramError::IdentityProgram { sequence_id: sequence.id, reason }
            })?;
        let shared_identity =
            PreparedEffectProgram::prepare_with_lut_cache(&[], &[], working_color_space, lut_cache)
                .map_err(|error| PreparedVisualProgramError::IdentityProgram {
                    sequence_id: sequence.id,
                    reason: error.to_string(),
                })?;

        let timeline_grade = match sequence.timeline_grade {
            None => PreparedTimelineGrade::None,
            Some(definition_id) => match prepared_grades.get(&definition_id) {
                Some(PreparedSharedGrade::Ready(grade)) => {
                    match PreparedEffectProgram::prepare_hierarchical_with_lut_cache(
                        &[],
                        &[],
                        &[],
                        std::slice::from_ref(grade),
                        working_color_space,
                        lut_cache,
                    ) {
                        Ok(program) => PreparedTimelineGrade::Ready(program),
                        Err(error) => PreparedTimelineGrade::Blocked {
                            reason: Arc::from(error.to_string()),
                            retry: if error.dependency_refresh_retryable() {
                                PreparedVisualBlockerRetry::ExternalChange
                            } else {
                                PreparedVisualBlockerRetry::AuthorOrDefinitionChange
                            },
                        },
                    }
                }
                Some(PreparedSharedGrade::Blocked { reason, retry }) => {
                    PreparedTimelineGrade::Blocked { reason: Arc::clone(reason), retry: *retry }
                }
                None => PreparedTimelineGrade::Blocked {
                    reason: Arc::from(format!(
                        "timeline references missing grade definition {definition_id}"
                    )),
                    retry: PreparedVisualBlockerRetry::AuthorOrDefinitionChange,
                },
            },
        };

        for track in
            sequence.video_tracks.iter().filter(|track| track.is_visible && !track.is_muted)
        {
            for clip in track.clips.iter().filter(|clip| !clip.is_disabled) {
                let (grade_before, grade_after) =
                    match prepared_clip_grade_segments(sequence, clip, &prepared_grades) {
                        Ok(segments) => segments,
                        Err((reason, retry)) => {
                            blocked_clips += 1;
                            clip_effects
                                .insert(clip.id, PreparedClipEffects::Blocked { reason, retry });
                            continue;
                        }
                    };
                let has_processing = clip.effects.iter().any(|effect| effect.is_enabled)
                    || clip.masks.iter().any(|mask| mask.enabled)
                    || !grade_before.is_empty()
                    || !grade_after.is_empty();
                let author_fingerprint = if has_processing {
                    hierarchical_clip_author_fingerprint(sequence, clip, working_color_space)
                        .map_err(|reason| PreparedVisualProgramError::AuthorFingerprint {
                            sequence_id: sequence.id,
                            clip_id: clip.id,
                            reason,
                        })?
                } else {
                    identity_author_fingerprint
                };
                let reusable = previous
                    .and_then(|program| program.clip_effects.get(&clip.id))
                    .and_then(|effects| match effects {
                        PreparedClipEffects::Ready {
                            program,
                            author_fingerprint: previous_fingerprint,
                        } if *previous_fingerprint == author_fingerprint
                            && !program.has_external_dependencies() =>
                        {
                            Some(program.clone())
                        }
                        PreparedClipEffects::Ready { .. } | PreparedClipEffects::Blocked { .. } => {
                            None
                        }
                    });
                let result = if let Some(program) = reusable {
                    reused_clips += 1;
                    Ok(program)
                } else if has_processing {
                    PreparedEffectProgram::prepare_hierarchical_with_lut_cache(
                        &clip.effects,
                        &clip.masks,
                        &grade_before,
                        &grade_after,
                        working_color_space,
                        lut_cache,
                    )
                } else {
                    Ok(shared_identity.clone())
                };
                let prepared = match result {
                    Ok(program) => {
                        prepared_clips += 1;
                        PreparedClipEffects::Ready { program, author_fingerprint }
                    }
                    Err(error) => {
                        blocked_clips += 1;
                        let retry = if error.dependency_refresh_retryable() {
                            PreparedVisualBlockerRetry::ExternalChange
                        } else {
                            PreparedVisualBlockerRetry::AuthorOrDefinitionChange
                        };
                        PreparedClipEffects::Blocked { reason: Arc::from(error.to_string()), retry }
                    }
                };
                if clip_effects.insert(clip.id, prepared).is_some() {
                    return Err(PreparedVisualProgramError::DuplicateClipId {
                        sequence_id: sequence.id,
                        clip_id: clip.id,
                    });
                }
            }
        }

        for transition in sequence.video_transitions.iter().filter(|transition| {
            transition.is_enabled
                && sequence.video_tracks.iter().any(|track| {
                    track.is_visible
                        && !track.is_muted
                        && track.clips.iter().any(|clip| clip.id == transition.left)
                })
        }) {
            let prepared = match &transition.transition_type {
                VideoTransitionType::CrossDissolve
                    if transition.properties.iter().next().is_none()
                        && transition
                            .params
                            .as_object()
                            .is_some_and(|params| params.is_empty()) =>
                {
                    prepared_transitions += 1;
                    PreparedVisualTransition::Ready
                }
                VideoTransitionType::CrossDissolve => {
                    blocked_transitions += 1;
                    PreparedVisualTransition::Blocked(Arc::from(
                        "Cross Dissolve carries unsupported definition state",
                    ))
                }
                VideoTransitionType::Plugin { definition_id } => {
                    blocked_transitions += 1;
                    PreparedVisualTransition::Blocked(Arc::from(format!(
                        "requires unavailable definition `{definition_id}`"
                    )))
                }
            };
            if transitions.insert(transition.id, prepared).is_some() {
                return Err(PreparedVisualProgramError::DuplicateTransitionId {
                    sequence_id: sequence.id,
                    transition_id: transition.id,
                });
            }
        }

        let final_registry_revision = effect_registry_revision();
        if final_registry_revision != key.effect_registry_revision {
            return Err(PreparedVisualProgramError::EffectRegistryChanged {
                sequence_id: sequence.id,
                before: key.effect_registry_revision,
                after: final_registry_revision,
            });
        }

        let retained_bytes_estimate = prepared_visual_program_retained_bytes_estimate(
            sequence,
            schedule.diagnostics(),
            &clip_effects,
            &transitions,
        );
        Ok(Self {
            key,
            materialization: PreparedVisualMaterializationContract {
                author_resolution: sequence.settings.resolution,
                authored_preview_resolution_scale: sequence.settings.preview.resolution_scale,
                title_safe_margin: sequence.settings.title_safe_margin,
            },
            schedule,
            clip_effects,
            timeline_grade,
            transitions,
            diagnostics: PreparedVisualProgramDiagnostics {
                sequence_id: sequence.id,
                sequence_revision: sequence.revision,
                effect_registry_revision: key.effect_registry_revision,
                prepared_clips,
                reused_clips,
                blocked_clips,
                prepared_transitions,
                blocked_transitions,
            },
            retained_bytes_estimate,
        })
    }

    /// Placement schedule owned by this exact execution program.
    pub fn schedule(&self) -> &PreparedVisualSchedule {
        &self.schedule
    }

    /// Exact Sequence identity.
    pub const fn sequence_id(&self) -> SequenceId {
        self.key.sequence_id
    }

    /// Exact conservative author revision.
    pub const fn sequence_revision(&self) -> SequenceRevision {
        self.key.sequence_revision
    }

    /// Exact frame time base of the prepared Sequence Evaluation Grid.
    pub fn evaluation_time_base(&self) -> Rational {
        self.schedule.source_time_base()
    }

    /// Static canvas/title facts frozen with this Program.
    pub const fn materialization_contract(&self) -> PreparedVisualMaterializationContract {
        self.materialization
    }

    /// Resolve one exact Clip-local temporal request through the immutable
    /// placement and retime snapshot owned by this program.
    pub fn sample_clip_source(
        &self,
        placement: TimelineClipExecutionRef,
        requested_clip_time: TimelineTime,
    ) -> Result<mondrian_core::SourceSampleTarget> {
        self.schedule.sample_clip_source(placement, requested_clip_time)
    }

    /// Freeze one Clip's exact time-expanded Effect execution through the
    /// immutable program and placement snapshot owned here.
    pub(crate) fn prepare_clip_temporal_execution(
        &self,
        placement: TimelineClipExecutionRef,
        request: &EffectTemporalExecutionRequest,
    ) -> Result<PreparedEffectTemporalExecution> {
        let effect_program = self.temporal_effect_program(placement, request)?;
        prepare_temporal_frame_execution(effect_program, request, |clip_time| {
            self.temporal_frame_seed(placement, clip_time)
        })
        .map_err(|error| self.temporal_preparation_error(placement.clip_id, error))
    }

    /// Freeze one Clip's time-expanded execution while sharing bounded dynamic
    /// topology residency with this consumer's ordinary frame evaluation.
    pub(crate) fn prepare_clip_temporal_execution_with_session(
        &self,
        placement: TimelineClipExecutionRef,
        request: &EffectTemporalExecutionRequest,
        session: &mut EffectExecutionSession,
    ) -> Result<PreparedEffectTemporalExecution> {
        let effect_program = self.temporal_effect_program(placement, request)?;
        session
            .prepare_temporal_frame_execution(effect_program, request, |clip_time| {
                self.temporal_frame_seed(placement, clip_time)
            })
            .map_err(|error| self.temporal_preparation_error(placement.clip_id, error))
    }

    fn temporal_effect_program(
        &self,
        placement: TimelineClipExecutionRef,
        request: &EffectTemporalExecutionRequest,
    ) -> Result<&PreparedEffectProgram> {
        if request.output_time() != placement.clip_time {
            return Err(MondrianError::EffectGraphEvaluationFailed {
                reason: format!(
                    "Clip {} temporal request time {:?} does not match prepared placement time {:?}",
                    placement.clip_id,
                    request.output_time(),
                    placement.clip_time
                ),
            });
        }
        let effect_program = match self.clip_effects.get(&placement.clip_id) {
            Some(PreparedClipEffects::Ready { program, .. }) => program,
            Some(PreparedClipEffects::Blocked { reason, .. }) => {
                return Err(MondrianError::EffectGraphEvaluationFailed {
                    reason: format!(
                        "Clip {} Effect preparation failed: {reason}",
                        placement.clip_id
                    ),
                });
            }
            None => {
                return Err(MondrianError::EffectGraphEvaluationFailed {
                    reason: format!(
                        "Clip {} is absent from prepared Sequence {} revision {:?}",
                        placement.clip_id, self.key.sequence_id, self.key.sequence_revision
                    ),
                });
            }
        };
        Ok(effect_program)
    }

    fn temporal_frame_seed(
        &self,
        placement: TimelineClipExecutionRef,
        clip_time: TimelineTime,
    ) -> std::result::Result<i64, String> {
        let sequence_time = self
            .schedule
            .clip_to_sequence_time(placement, clip_time)
            .map_err(|error| error.to_string())?;
        Ok(visual_frame_seed(
            sequence_time,
            self.evaluation_time_base(),
        ))
    }

    fn temporal_preparation_error(
        &self,
        clip_id: ClipId,
        error: mondrian_effects::EffectTemporalExecutionError,
    ) -> MondrianError {
        MondrianError::EffectGraphEvaluationFailed {
            reason: format!(
                "Clip {} temporal Effect preparation failed: {error}",
                clip_id
            ),
        }
    }

    /// Exact Effect-definition registry revision.
    pub const fn effect_registry_revision(&self) -> u64 {
        self.key.effect_registry_revision
    }

    /// Versioned conservative identity of the exact visual author projection
    /// compiled by this Program.
    pub const fn visual_author_fingerprint(&self) -> [u8; 32] {
        self.key.visual_author_fingerprint
    }

    /// Static preparation evidence.
    pub const fn diagnostics(&self) -> PreparedVisualProgramDiagnostics {
        self.diagnostics
    }

    /// Conservative logical bytes charged to a visual-program residency grant.
    ///
    /// This is deliberately an admission estimate rather than allocator
    /// telemetry. Shared `Arc` payloads may be counted more than once so the
    /// estimate cannot silently authorize an unbounded long-program cache.
    pub const fn retained_bytes_estimate(&self) -> usize {
        self.retained_bytes_estimate
    }

    /// Per-Clip ordered execution envelope when that Clip prepared
    /// successfully.
    pub fn clip_execution_envelope(&self, clip_id: ClipId) -> Option<&EffectExecutionEnvelope> {
        match self.clip_effects.get(&clip_id) {
            Some(PreparedClipEffects::Ready { program, .. }) => Some(program.execution_envelope()),
            Some(PreparedClipEffects::Blocked { .. }) | None => None,
        }
    }

    /// Return all fail-closed Clip blockers in deterministic Clip-ID order.
    pub fn blockers(&self) -> Vec<PreparedVisualEffectBlocker> {
        let mut blockers = self
            .clip_effects
            .iter()
            .filter_map(|(clip_id, program)| match program {
                PreparedClipEffects::Ready { .. } => None,
                PreparedClipEffects::Blocked { reason, .. } => Some(PreparedVisualEffectBlocker {
                    clip_id: *clip_id,
                    reason: Arc::clone(reason),
                }),
            })
            .collect::<Vec<_>>();
        blockers.sort_by_key(|blocker| blocker.clip_id.to_string());
        blockers
    }

    /// Return all fail-closed Transition blockers in deterministic identity
    /// order.
    pub fn transition_blockers(&self) -> Vec<PreparedVisualTransitionBlocker> {
        let mut blockers = self
            .transitions
            .iter()
            .filter_map(|(transition_id, transition)| match transition {
                PreparedVisualTransition::Ready => None,
                PreparedVisualTransition::Blocked(reason) => {
                    Some(PreparedVisualTransitionBlocker {
                        transition_id: *transition_id,
                        reason: Arc::clone(reason),
                    })
                }
            })
            .collect::<Vec<_>>();
        blockers.sort_by_key(|blocker| blocker.transition_id.to_string());
        blockers
    }

    /// Conservatively fail closed if any visible Clip or Transition in the
    /// complete Sequence could not be prepared.
    ///
    /// Export must use [`Self::preflight_frame`] over its selected range
    /// instead. This whole-Sequence diagnostic is retained for callers that
    /// deliberately need a conservative readiness summary.
    pub fn preflight(&self) -> Result<()> {
        self.ensure_timeline_grade_ready()?;
        if let Some(blocker) = self.blockers().into_iter().next() {
            return Err(MondrianError::EffectGraphEvaluationFailed {
                reason: format!(
                    "Clip {} visual Effect preparation failed: {}",
                    blocker.clip_id, blocker.reason
                ),
            });
        }
        if let Some(blocker) = self.transition_blockers().into_iter().next() {
            return Err(transition_blocker_error(
                blocker.transition_id,
                blocker.reason.as_ref(),
            ));
        }
        Ok(())
    }

    /// Preflight only visual work reachable at one exact Sequence frame.
    ///
    /// This checks active Clip Effect and Transition-definition blockers
    /// without evaluating pixels or dynamic Effect graphs. The returned nested
    /// demands preserve exact child-local time so an Export Adapter can recurse
    /// on each referenced child Sequence's own Evaluation Grid.
    pub fn preflight_frame(&self, timeline_frame: i64) -> Result<PreparedVisualFrameReachability> {
        let time = TimelineTime::from_frame_position(FramePosition::new(
            timeline_frame,
            self.schedule.source_time_base(),
        ))?;
        self.preflight_time(time)
    }

    /// Preflight only visual work reachable at one exact Sequence-local time.
    pub fn preflight_time(&self, time: TimelineTime) -> Result<PreparedVisualFrameReachability> {
        self.ensure_timeline_grade_ready()?;
        let items = self.schedule.flat_visual_items_at(time)?;
        let mut reachability = PreparedVisualFrameReachability::default();
        for item in &items {
            match item {
                FlatVisualItem::Clip(clip) => {
                    self.preflight_clip(clip, &mut reachability)?;
                }
                FlatVisualItem::Transition(transition) => {
                    self.ensure_transition_ready(transition.transition_id)?;
                    self.preflight_clip(&transition.left, &mut reachability)?;
                    self.preflight_clip(&transition.right, &mut reachability)?;
                }
            }
        }
        Ok(reachability)
    }

    /// Preflight immutable media dependencies over one inclusive Sequence-time
    /// window without enumerating frames.
    ///
    /// Visibility, mute, disabled placement, Transition endpoint, and retime
    /// semantics come exclusively from the prepared schedule. Finite temporal
    /// Effect extent expands nested child demands; an unbounded extent
    /// deliberately promotes the child to whole-Sequence reachability. A
    /// blocked Effect program is likewise treated as unbounded here; executable
    /// visual preflight reports that independent blocker at its normal seam.
    pub fn preflight_range_dependencies(
        &self,
        first: TimelineTime,
        last: TimelineTime,
    ) -> Result<PreparedVisualRangeReachability> {
        let clips = self.schedule.range_clips(first, last)?;
        let mut reachability = PreparedVisualRangeReachability::default();
        for clip in clips {
            match clip.placement.endpoint {
                TimelineClipEndpointContext::TransitionLeft { transition_id }
                | TimelineClipEndpointContext::TransitionRight { transition_id } => {
                    reachability.transition_ids.push(transition_id);
                }
                TimelineClipEndpointContext::Ordinary => {}
            }
            if let Some(asset_id) = clip.content.media_asset_id() {
                reachability.media_asset_ids.push(asset_id);
            }
            if let ClipContent::BasicTitle { title } = &clip.content {
                title.validate_author_state()?;
                reachability.basic_title_font_queries.push(BasicTitleFontQuery::from_title(
                    &title.evaluate(TimelineTime::ZERO)?,
                ));
            }
            let ClipContent::NestedSequence { sequence_id, .. } = clip.content else {
                continue;
            };
            let temporal = self
                .clip_execution_envelope(clip.placement.clip_id)
                .map(|envelope| envelope.aggregate().temporal_input)
                .unwrap_or(mondrian_effects::EffectTemporalInputExtent::UNBOUNDED);
            if matches!(temporal.past, EffectTemporalSpan::Unbounded)
                || matches!(temporal.future, EffectTemporalSpan::Unbounded)
            {
                reachability.nested_demands.push(PreparedVisualNestedRangeDemand {
                    sequence_id,
                    range: PreparedVisualNestedRange::WholeSequence,
                });
                continue;
            }
            let mut first_clip_time = clip.first_clip_time.min(clip.last_clip_time);
            let mut last_clip_time = clip.first_clip_time.max(clip.last_clip_time);
            if let EffectTemporalSpan::Finite(past) = temporal.past {
                first_clip_time = first_clip_time.checked_sub(past)?;
            }
            if let EffectTemporalSpan::Finite(future) = temporal.future {
                last_clip_time = last_clip_time.checked_add(future)?;
            }
            let first_source = self.sample_clip_source(clip.placement, first_clip_time)?;
            let last_source = self.sample_clip_source(clip.placement, last_clip_time)?;
            reachability.nested_demands.push(PreparedVisualNestedRangeDemand {
                sequence_id,
                range: PreparedVisualNestedRange::Bounded {
                    first: first_source.time().min(last_source.time()),
                    last: first_source.time().max(last_source.time()),
                },
            });
        }
        reachability.media_asset_ids.sort_unstable_by_key(ToString::to_string);
        reachability.media_asset_ids.dedup();
        reachability.transition_ids.sort_unstable_by_key(ToString::to_string);
        reachability.transition_ids.dedup();
        reachability.basic_title_font_queries.sort();
        reachability.basic_title_font_queries.dedup();
        reachability.nested_demands.sort_unstable_by_key(|demand| {
            (
                demand.sequence_id.to_string(),
                match demand.range {
                    PreparedVisualNestedRange::Bounded { first, last } => {
                        format!("0:{first:?}:{last:?}")
                    }
                    PreparedVisualNestedRange::WholeSequence => "1".to_owned(),
                },
            )
        });
        reachability.nested_demands.dedup();
        Ok(reachability)
    }

    pub(crate) fn evaluate_clip_effects(
        &self,
        clip_id: ClipId,
        clip_time: TimelineTime,
    ) -> Result<Arc<CompiledEffectGraph>> {
        match self.clip_effects.get(&clip_id) {
            Some(PreparedClipEffects::Ready { program, .. }) => program
                .evaluate(clip_time)
                .map_err(|error| MondrianError::EffectGraphEvaluationFailed {
                    reason: format!("Clip {clip_id} Effect evaluation failed: {error}"),
                }),
            Some(PreparedClipEffects::Blocked { reason, .. }) => {
                Err(MondrianError::EffectGraphEvaluationFailed {
                    reason: format!("Clip {clip_id} Effect preparation failed: {reason}"),
                })
            }
            None => Err(MondrianError::EffectGraphEvaluationFailed {
                reason: format!(
                    "Clip {clip_id} is absent from prepared Sequence {} revision {:?}",
                    self.key.sequence_id, self.key.sequence_revision
                ),
            }),
        }
    }

    pub(crate) fn evaluate_clip_effects_with_session(
        &self,
        clip_id: ClipId,
        clip_time: TimelineTime,
        session: &mut EffectExecutionSession,
    ) -> Result<Arc<CompiledEffectGraph>> {
        match self.clip_effects.get(&clip_id) {
            Some(PreparedClipEffects::Ready { program, .. }) => program
                .evaluate_with_session(clip_time, session)
                .map_err(|error| MondrianError::EffectGraphEvaluationFailed {
                    reason: format!("Clip {clip_id} Effect evaluation failed: {error}"),
                }),
            Some(PreparedClipEffects::Blocked { reason, .. }) => {
                Err(MondrianError::EffectGraphEvaluationFailed {
                    reason: format!("Clip {clip_id} Effect preparation failed: {reason}"),
                })
            }
            None => Err(MondrianError::EffectGraphEvaluationFailed {
                reason: format!(
                    "Clip {clip_id} is absent from prepared Sequence {} revision {:?}",
                    self.key.sequence_id, self.key.sequence_revision
                ),
            }),
        }
    }

    pub(crate) fn evaluate_timeline_grade(
        &self,
        sequence_time: TimelineTime,
    ) -> Result<Option<Arc<CompiledEffectGraph>>> {
        match &self.timeline_grade {
            PreparedTimelineGrade::None => Ok(None),
            PreparedTimelineGrade::Ready(program) => program
                .evaluate(sequence_time)
                .map(Some)
                .map_err(|error| MondrianError::EffectGraphEvaluationFailed {
                    reason: format!("Timeline Grade evaluation failed: {error}"),
                }),
            PreparedTimelineGrade::Blocked { reason, .. } => {
                Err(MondrianError::EffectGraphEvaluationFailed {
                    reason: format!("Timeline Grade preparation failed: {reason}"),
                })
            }
        }
    }

    pub(crate) fn evaluate_timeline_grade_with_session(
        &self,
        sequence_time: TimelineTime,
        session: &mut EffectExecutionSession,
    ) -> Result<Option<Arc<CompiledEffectGraph>>> {
        match &self.timeline_grade {
            PreparedTimelineGrade::None => Ok(None),
            PreparedTimelineGrade::Ready(program) => program
                .evaluate_with_session(sequence_time, session)
                .map(Some)
                .map_err(|error| MondrianError::EffectGraphEvaluationFailed {
                    reason: format!("Timeline Grade evaluation failed: {error}"),
                }),
            PreparedTimelineGrade::Blocked { reason, .. } => {
                Err(MondrianError::EffectGraphEvaluationFailed {
                    reason: format!("Timeline Grade preparation failed: {reason}"),
                })
            }
        }
    }

    fn ensure_timeline_grade_ready(&self) -> Result<()> {
        match &self.timeline_grade {
            PreparedTimelineGrade::None | PreparedTimelineGrade::Ready(_) => Ok(()),
            PreparedTimelineGrade::Blocked { reason, .. } => {
                Err(MondrianError::EffectGraphEvaluationFailed {
                    reason: format!("Timeline Grade preparation failed: {reason}"),
                })
            }
        }
    }

    pub(crate) fn ensure_transition_ready(&self, transition_id: VideoTransitionId) -> Result<()> {
        match self.transitions.get(&transition_id) {
            Some(PreparedVisualTransition::Ready) => Ok(()),
            Some(PreparedVisualTransition::Blocked(reason)) => {
                Err(transition_blocker_error(transition_id, reason))
            }
            None => Err(MondrianError::WorkflowStepFailed {
                step_id: "compile_video_transition".to_owned(),
                reason: format!(
                    "video Transition {transition_id} is absent from prepared Sequence {} revision {:?}",
                    self.key.sequence_id, self.key.sequence_revision
                ),
            }),
        }
    }

    fn preflight_clip(
        &self,
        clip: &FlatActiveClip,
        reachability: &mut PreparedVisualFrameReachability,
    ) -> Result<()> {
        if clip.is_disabled || clip.opacity.clamp(0.0, 1.0) <= 0.0 {
            return Ok(());
        }
        self.ensure_clip_ready(clip.clip_id)?;
        if let ClipContent::NestedSequence { sequence_id, .. } = &clip.content {
            reachability.nested_demands.push(PreparedVisualNestedDemand {
                sequence_id: *sequence_id,
                source_sample: clip.source_sample,
            });
        }
        Ok(())
    }

    fn ensure_clip_ready(&self, clip_id: ClipId) -> Result<()> {
        match self.clip_effects.get(&clip_id) {
            Some(PreparedClipEffects::Ready { .. }) => Ok(()),
            Some(PreparedClipEffects::Blocked { reason, .. }) => {
                Err(MondrianError::EffectGraphEvaluationFailed {
                    reason: format!("Clip {clip_id} Effect preparation failed: {reason}"),
                })
            }
            None => Err(MondrianError::EffectGraphEvaluationFailed {
                reason: format!(
                    "Clip {clip_id} is absent from prepared Sequence {} revision {:?}",
                    self.key.sequence_id, self.key.sequence_revision
                ),
            }),
        }
    }

    /// Report whether a low-frequency dependency observer should evict and
    /// rebuild this immutable program.
    ///
    /// This method may inspect prepared external resources and is therefore
    /// intended for a file-watch/manual-refresh Adapter, never per-frame
    /// evaluation. A definition-registry revision change, an
    /// external-change-retryable Clip blocker, or a stale prepared resource
    /// requests refresh. Author-edit and definition blockers wait for those
    /// explicit state changes instead of causing background rebuild churn.
    pub fn dependency_refresh_required(
        &self,
    ) -> std::result::Result<bool, PreparedVisualProgramDependencyError> {
        if effect_registry_revision() != self.key.effect_registry_revision {
            return Ok(true);
        }
        match &self.timeline_grade {
            PreparedTimelineGrade::Ready(program) => {
                if !program.dependencies_are_current().map_err(|error| {
                    PreparedVisualProgramDependencyError {
                        sequence_id: self.key.sequence_id,
                        clip_id: None,
                        reason: format!("Timeline Grade: {error}"),
                    }
                })? {
                    return Ok(true);
                }
            }
            PreparedTimelineGrade::Blocked {
                retry: PreparedVisualBlockerRetry::ExternalChange,
                ..
            } => return Ok(true),
            PreparedTimelineGrade::None
            | PreparedTimelineGrade::Blocked {
                retry: PreparedVisualBlockerRetry::AuthorOrDefinitionChange,
                ..
            } => {}
        }
        for (clip_id, effects) in &self.clip_effects {
            match effects {
                PreparedClipEffects::Ready { program, .. } => {
                    if !program.dependencies_are_current().map_err(|error| {
                        PreparedVisualProgramDependencyError {
                            sequence_id: self.key.sequence_id,
                            clip_id: Some(*clip_id),
                            reason: format!("Clip {clip_id}: {error}"),
                        }
                    })? {
                        return Ok(true);
                    }
                }
                PreparedClipEffects::Blocked {
                    retry: PreparedVisualBlockerRetry::ExternalChange,
                    ..
                } => return Ok(true),
                PreparedClipEffects::Blocked {
                    retry: PreparedVisualBlockerRetry::AuthorOrDefinitionChange,
                    ..
                } => {}
            }
        }
        Ok(false)
    }

    fn dependencies_are_current(
        &self,
    ) -> std::result::Result<bool, PreparedVisualProgramDependencyError> {
        self.dependency_refresh_required().map(|required| !required)
    }
}

/// Immutable proof that one prepared Program was checked against one exact
/// validated author snapshot.
///
/// Frame and range closure builders consume this binding instead of hashing
/// the complete Sequence author payload on every query. Only
/// [`PreparedVisualProgramCache::bind_author_snapshot`] and the checked
/// constructor can create a binding.
#[derive(Debug, Clone)]
pub struct PreparedVisualProgramBinding {
    author_snapshot: Option<PreparedVisualAuthorSnapshotIdentity>,
    program: Arc<PreparedVisualProgram>,
}

impl PreparedVisualProgramBinding {
    /// Validate a raw or independently assembled Sequence against a Program
    /// and create an unscoped binding.
    ///
    /// Production Preview should normally obtain bindings from
    /// [`PreparedVisualProgramCache::bind_author_snapshot`]. This checked
    /// Interface remains available for Export capture and scalar references
    /// that do not possess validated author-generation authority.
    pub fn checked(
        sequence: &Sequence,
        program: Arc<PreparedVisualProgram>,
    ) -> std::result::Result<Self, PreparedVisualProgramBindingError> {
        validate_program_identity(sequence, &program, true, false)?;
        Ok(Self { author_snapshot: None, program })
    }

    fn from_prepared(
        author_snapshot: PreparedVisualAuthorSnapshotIdentity,
        sequence: &Sequence,
        program: Arc<PreparedVisualProgram>,
    ) -> std::result::Result<Self, PreparedVisualProgramBindingError> {
        validate_program_identity(sequence, &program, false, true)?;
        Ok(Self { author_snapshot: Some(author_snapshot), program })
    }

    /// Validated immutable author snapshot that owns this binding.
    pub const fn author_snapshot(&self) -> Option<PreparedVisualAuthorSnapshotIdentity> {
        self.author_snapshot
    }

    /// Exact prepared Program retained by the binding.
    pub const fn program(&self) -> &Arc<PreparedVisualProgram> {
        &self.program
    }

    pub(crate) fn validate_for_sequence(
        &self,
        sequence: &Sequence,
    ) -> std::result::Result<(), PreparedVisualProgramBindingError> {
        validate_program_identity(sequence, &self.program, false, false)
    }
}

/// Failure to bind or consume an exact prepared visual Program.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Sequence {sequence_id} prepared visual Program binding is invalid: {reason}")]
pub struct PreparedVisualProgramBindingError {
    /// Sequence expected by the binding consumer.
    pub sequence_id: SequenceId,
    /// Stable identity or author-state mismatch diagnostic.
    pub reason: String,
}

fn validate_program_identity(
    sequence: &Sequence,
    program: &PreparedVisualProgram,
    validate_author_fingerprint: bool,
    validate_current_effect_registry: bool,
) -> std::result::Result<(), PreparedVisualProgramBindingError> {
    if program.sequence_id() != sequence.id || program.sequence_revision() != sequence.revision {
        return Err(PreparedVisualProgramBindingError {
            sequence_id: sequence.id,
            reason: format!(
                "expected identity/revision {} {:?}, received {} {:?}",
                sequence.id,
                sequence.revision,
                program.sequence_id(),
                program.sequence_revision()
            ),
        });
    }
    if validate_current_effect_registry
        && program.effect_registry_revision() != effect_registry_revision()
    {
        return Err(PreparedVisualProgramBindingError {
            sequence_id: sequence.id,
            reason: format!(
                "Effect registry revision changed (Program {}, current {})",
                program.effect_registry_revision(),
                effect_registry_revision()
            ),
        });
    }
    if validate_author_fingerprint {
        let expected = prepared_visual_author_fingerprint(sequence).map_err(|error| {
            PreparedVisualProgramBindingError {
                sequence_id: sequence.id,
                reason: error.to_string(),
            }
        })?;
        if program.visual_author_fingerprint() != expected {
            return Err(PreparedVisualProgramBindingError {
                sequence_id: sequence.id,
                reason: "conservative visual-author fingerprint does not match".to_owned(),
            });
        }
    }
    Ok(())
}

fn transition_blocker_error(
    transition_id: VideoTransitionId,
    reason: impl std::fmt::Display,
) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "compile_video_transition".to_owned(),
        reason: format!("video Transition {transition_id} {reason}"),
    }
}

fn prepared_visual_program_retained_bytes_estimate(
    sequence: &Sequence,
    schedule: mondrian_timeline::PreparedVisualScheduleDiagnostics,
    clip_effects: &HashMap<ClipId, PreparedClipEffects>,
    transitions: &HashMap<VideoTransitionId, PreparedVisualTransition>,
) -> usize {
    let author_payload_bytes =
        match serde_json::to_vec(&(&sequence.video_tracks, &sequence.video_transitions)) {
            Ok(payload) => payload.len(),
            Err(_) => usize::MAX,
        };
    let mut retained = std::mem::size_of::<PreparedVisualProgram>()
        .saturating_add(author_payload_bytes.saturating_mul(4))
        .saturating_add(
            schedule
                .video_tracks
                .saturating_mul(std::mem::size_of::<usize>().saturating_mul(24)),
        )
        .saturating_add(
            schedule
                .clip_intervals
                .saturating_mul(std::mem::size_of::<usize>().saturating_mul(32)),
        )
        .saturating_add(
            schedule
                .transition_intervals
                .saturating_mul(std::mem::size_of::<usize>().saturating_mul(24)),
        );
    for prepared in clip_effects.values() {
        retained = retained.saturating_add(match prepared {
            PreparedClipEffects::Ready { program, author_fingerprint } => program
                .retained_bytes_estimate()
                .saturating_add(author_fingerprint.len())
                .saturating_add(std::mem::size_of::<PreparedClipEffects>()),
            PreparedClipEffects::Blocked { reason, .. } => {
                reason.len().saturating_add(std::mem::size_of::<PreparedClipEffects>())
            }
        });
    }
    for transition in transitions.values() {
        retained = retained.saturating_add(match transition {
            PreparedVisualTransition::Ready => std::mem::size_of::<PreparedVisualTransition>(),
            PreparedVisualTransition::Blocked(reason) => {
                reason.len().saturating_add(std::mem::size_of::<PreparedVisualTransition>())
            }
        });
    }
    retained
}

fn clip_effect_author_fingerprint(
    effects: &[mondrian_core::effect_data::EffectNode],
    masks: &[mondrian_core::mask_data::MaskComponent],
    working_color_space: mondrian_core::WorkingColorSpace,
) -> std::result::Result<[u8; 32], String> {
    let canonical = serde_json::to_vec(&(effects, masks, working_color_space))
        .map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.visual-effect-author-fingerprint.v1");
    hasher.update((canonical.len() as u64).to_le_bytes());
    hasher.update(canonical);
    Ok(hasher.finalize().into())
}

/// Failure to build one immutable visual execution program.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PreparedVisualProgramError {
    /// The conservative visual author projection could not be encoded.
    #[error(transparent)]
    VisualAuthorFingerprint(#[from] PreparedVisualAuthorFingerprintError),
    /// Placement schedule preparation failed.
    #[error("Sequence {sequence_id} visual schedule preparation failed: {reason}")]
    Schedule {
        /// Sequence that failed.
        sequence_id: SequenceId,
        /// Underlying validated timeline error.
        reason: String,
    },
    /// The renderer could not create its shared source-only identity program.
    #[error("Sequence {sequence_id} identity Effect program preparation failed: {reason}")]
    IdentityProgram {
        /// Sequence that failed.
        sequence_id: SequenceId,
        /// Effect preparation diagnostic.
        reason: String,
    },
    /// Author state violated globally unique Clip identity.
    #[error("Sequence {sequence_id} contains duplicate Clip ID {clip_id}")]
    DuplicateClipId {
        /// Sequence that failed.
        sequence_id: SequenceId,
        /// Repeated Clip identity.
        clip_id: ClipId,
    },
    /// Author state violated globally unique visual Transition identity.
    #[error("Sequence {sequence_id} contains duplicate visual Transition ID {transition_id}")]
    DuplicateTransitionId {
        /// Sequence that failed.
        sequence_id: SequenceId,
        /// Repeated Transition identity.
        transition_id: VideoTransitionId,
    },
    /// Complete Effect/Mask author semantics could not be canonically encoded.
    #[error("Sequence {sequence_id} Clip {clip_id} Effect author fingerprint failed: {reason}")]
    AuthorFingerprint {
        /// Sequence that failed.
        sequence_id: SequenceId,
        /// Clip whose author state could not be encoded.
        clip_id: ClipId,
        /// Canonical encoding diagnostic.
        reason: String,
    },
    /// Definitions changed while immutable programs were being bound.
    #[error(
        "Sequence {sequence_id} Effect registry changed during preparation ({before} -> {after})"
    )]
    EffectRegistryChanged {
        /// Sequence that failed.
        sequence_id: SequenceId,
        /// Revision sampled before preparation.
        before: u64,
        /// Revision sampled after preparation.
        after: u64,
    },
}

/// Failure to bind one validated immutable author snapshot to resident visual
/// execution state.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PreparedVisualProgramBindError {
    /// Program preparation failed.
    #[error(transparent)]
    Preparation(#[from] PreparedVisualProgramError),
    /// The resulting Program did not match the supplied author snapshot.
    #[error(transparent)]
    Binding(#[from] PreparedVisualProgramBindingError),
    /// The exact Program exceeded the cache's active residency grant.
    ///
    /// The failure is retained for the same author snapshot, Sequence
    /// revision, and Effect registry revision so a frame loop cannot repeatedly
    /// compile an unadmittable Program.
    #[error(
        "Sequence {sequence_id} prepared visual Program requires {required_bytes} bytes, exceeding the {maximum_bytes}-byte residency grant"
    )]
    ResidencyRejected {
        /// Sequence whose mandatory Program could not be retained.
        sequence_id: SequenceId,
        /// Conservative stable Program charge.
        required_bytes: usize,
        /// Current Program-cache byte grant.
        maximum_bytes: usize,
    },
}

/// Explicit low-frequency dependency revalidation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Sequence {sequence_id} visual dependency revalidation failed: {reason}")]
pub struct PreparedVisualProgramDependencyError {
    /// Prepared Sequence.
    pub sequence_id: SequenceId,
    /// Clip that owns the resource, or `None` for the Timeline Grade.
    pub clip_id: Option<ClipId>,
    /// Resource Adapter diagnostic.
    pub reason: String,
}

/// Current and cumulative evidence for one bounded visual-program cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PreparedVisualProgramCacheDiagnostics {
    /// Current retained programs.
    pub entries: usize,
    /// Configured maximum retained programs.
    pub max_entries: usize,
    /// Conservative logical bytes retained by current programs.
    pub retained_bytes: usize,
    /// Configured conservative logical byte budget.
    pub max_retained_bytes: usize,
    /// Exact Sequence/definition revision hits.
    pub hits: u64,
    /// Successfully prepared cache misses.
    pub misses: u64,
    /// Programs evicted by capacity, revision, or explicit invalidation.
    pub evictions: u64,
    /// Prepared programs returned to the caller but not retained because one
    /// program exceeded the current grant or retention was disabled.
    pub rejected_residency: u64,
    /// Authoring/Open-lifetime scope rotations.
    pub scope_rotations: u64,
    /// Exact validated author-snapshot binding hits that avoided canonical
    /// author serialization and Program-cache lookup.
    pub author_snapshot_binding_hits: u64,
    /// Exact validated author-snapshot binding misses.
    pub author_snapshot_binding_misses: u64,
    /// Author-snapshot changes that retired the prior hot binding set.
    pub author_snapshot_binding_rotations: u64,
    /// Canonical visual-author fingerprints computed by this cache.
    pub author_fingerprint_evaluations: u64,
    /// Repeated exact-snapshot preparation failures served without retrying
    /// preparation on the frame path.
    pub author_snapshot_failure_hits: u64,
}

/// Entry and conservative logical-byte grant for one visual-program owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedVisualProgramCacheConfig {
    /// Maximum exact Sequence/definition revisions retained.
    pub max_entries: usize,
    /// Maximum aggregate conservative logical bytes retained.
    pub max_retained_bytes: usize,
    /// Owner-local prepared LUT entry/byte grant.
    pub lut_cache: LutPreparationCacheConfig,
}

impl PreparedVisualProgramCacheConfig {
    /// Construct an explicit two-dimensional residency grant.
    pub const fn new(max_entries: usize, max_retained_bytes: usize) -> Self {
        Self {
            max_entries,
            max_retained_bytes,
            lut_cache: LutPreparationCacheConfig::new(8, 64 * 1024 * 1024),
        }
    }

    /// Replace the owner-local LUT preparation grant.
    pub const fn with_lut_cache(mut self, lut_cache: LutPreparationCacheConfig) -> Self {
        self.lut_cache = lut_cache;
        self
    }
}

impl Default for PreparedVisualProgramCacheConfig {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_PREPARED_VISUAL_PROGRAM_CACHE_CAPACITY,
            max_retained_bytes: DEFAULT_PREPARED_VISUAL_PROGRAM_CACHE_BYTES,
            lut_cache: LutPreparationCacheConfig::new(8, 64 * 1024 * 1024),
        }
    }
}

#[derive(Debug, Clone)]
struct AuthorSnapshotProgramBinding {
    key: ScopedPreparedVisualProgramKey,
    binding: PreparedVisualProgramBinding,
}

#[derive(Debug, Clone)]
struct AuthorSnapshotBindingFailure {
    sequence_revision: SequenceRevision,
    effect_registry_revision: u64,
    error: PreparedVisualProgramBindError,
}

/// Consumer-owned, bounded cache of immutable visual execution programs.
///
/// Raw [`Self::prepare`] lookups validate the Sequence revision, its versioned
/// visual-author fingerprint, and the Effect-registry revision. Validated
/// author-snapshot bindings compute that fingerprint only on the first lookup
/// for each Sequence in a generation, then validate immutable snapshot,
/// revision, registry, cache scope, and resident Program identity in constant
/// time. Revisions remain meaningful only inside one caller-defined
/// Authoring/Open lifetime. A long-lived consumer must call
/// [`Self::rotate_scope`] before evaluating another authoring lifetime, even
/// when durable Project, Sequence, Track, and Clip IDs happen to match.
///
/// External files are revalidated only through
/// [`Self::revalidate_dependencies`], which is intentionally a low-frequency
/// file-watch/manual-refresh Seam and must not run once per frame.
pub struct PreparedVisualProgramCache {
    config: PreparedVisualProgramCacheConfig,
    lut_cache: LutPreparationCache,
    retained_bytes: usize,
    scope_generation: u64,
    entries: HashMap<ScopedPreparedVisualProgramKey, Arc<PreparedVisualProgram>>,
    recency: VecDeque<ScopedPreparedVisualProgramKey>,
    hits: u64,
    misses: u64,
    evictions: u64,
    rejected_residency: u64,
    scope_rotations: u64,
    author_snapshot: Option<PreparedVisualAuthorSnapshotIdentity>,
    author_snapshot_bindings: HashMap<SequenceId, AuthorSnapshotProgramBinding>,
    author_snapshot_failures: HashMap<SequenceId, AuthorSnapshotBindingFailure>,
    author_snapshot_binding_hits: u64,
    author_snapshot_binding_misses: u64,
    author_snapshot_binding_rotations: u64,
    author_fingerprint_evaluations: u64,
    author_snapshot_failure_hits: u64,
}

impl PreparedVisualProgramCache {
    /// Construct a bounded cache with the default byte grant.
    pub fn new(capacity: usize) -> Self {
        Self::with_config(PreparedVisualProgramCacheConfig {
            max_entries: capacity,
            ..PreparedVisualProgramCacheConfig::default()
        })
    }

    /// Construct a cache from an explicit entry-and-byte grant.
    pub fn with_config(config: PreparedVisualProgramCacheConfig) -> Self {
        Self {
            config,
            lut_cache: LutPreparationCache::new(config.lut_cache),
            retained_bytes: 0,
            scope_generation: 0,
            entries: HashMap::new(),
            recency: VecDeque::new(),
            hits: 0,
            misses: 0,
            evictions: 0,
            rejected_residency: 0,
            scope_rotations: 0,
            author_snapshot: None,
            author_snapshot_bindings: HashMap::new(),
            author_snapshot_failures: HashMap::new(),
            author_snapshot_binding_hits: 0,
            author_snapshot_binding_misses: 0,
            author_snapshot_binding_rotations: 0,
            author_fingerprint_evaluations: 0,
            author_snapshot_failure_hits: 0,
        }
    }

    /// Apply a new grant online, trim residency, and retire grant-bound
    /// negative admission evidence.
    pub fn reconfigure(&mut self, config: PreparedVisualProgramCacheConfig) {
        if self.config == config {
            return;
        }
        self.config = config;
        self.lut_cache.reconfigure(config.lut_cache);
        self.trim_to_config();
        self.author_snapshot_failures.retain(|_, failure| {
            !matches!(
                &failure.error,
                PreparedVisualProgramBindError::ResidencyRejected { .. }
            )
        });
    }

    /// Start a fresh Authoring/Open lifetime and retire every prior program.
    ///
    /// Scope rotation and residency retirement are one cache operation. The
    /// scoped address also prevents old entries from authorizing a hit if the
    /// clearing implementation is changed later.
    pub fn rotate_scope(&mut self) {
        self.scope_generation = self.scope_generation.wrapping_add(1);
        self.scope_rotations = self.scope_rotations.saturating_add(1);
        self.evictions = self.evictions.saturating_add(self.entries.len() as u64);
        self.entries.clear();
        self.recency.clear();
        self.retained_bytes = 0;
        self.lut_cache.clear();
        self.author_snapshot = None;
        self.author_snapshot_bindings.clear();
        self.author_snapshot_failures.clear();
    }

    /// Bind one exact validated author snapshot to a resident Program.
    ///
    /// The first request in an author generation performs the conservative
    /// fingerprint and ordinary Program-cache admission. Later current-frame,
    /// prefetch, and range queries reuse this binding without serializing the
    /// complete Sequence again. Sequence revision, Effect-registry revision,
    /// cache scope, and exact resident Program identity are still checked on
    /// every hit.
    pub fn bind_author_snapshot(
        &mut self,
        author_snapshot: PreparedVisualAuthorSnapshotIdentity,
        sequence: &Sequence,
    ) -> std::result::Result<PreparedVisualProgramBinding, PreparedVisualProgramBindError> {
        self.activate_author_snapshot(author_snapshot);
        let registry_revision = effect_registry_revision();

        if let Some(cached) = self.author_snapshot_bindings.get(&sequence.id).cloned() {
            let exact_resident = cached.key.scope_generation == self.scope_generation
                && cached.key.program.sequence_revision == sequence.revision
                && cached.key.program.effect_registry_revision == registry_revision
                && self
                    .entries
                    .get(&cached.key)
                    .is_some_and(|resident| Arc::ptr_eq(resident, cached.binding.program()))
                && cached.binding.author_snapshot() == Some(author_snapshot)
                && cached.binding.validate_for_sequence(sequence).is_ok();
            if exact_resident {
                self.author_snapshot_binding_hits =
                    self.author_snapshot_binding_hits.saturating_add(1);
                self.touch(cached.key);
                return Ok(cached.binding);
            }
            self.author_snapshot_bindings.remove(&sequence.id);
        }

        if let Some(failure) = self.author_snapshot_failures.get(&sequence.id)
            && failure.sequence_revision == sequence.revision
            && failure.effect_registry_revision == registry_revision
        {
            self.author_snapshot_failure_hits = self.author_snapshot_failure_hits.saturating_add(1);
            return Err(failure.error.clone());
        }
        self.author_snapshot_failures.remove(&sequence.id);
        self.author_snapshot_binding_misses = self.author_snapshot_binding_misses.saturating_add(1);

        let program = match self.prepare(sequence) {
            Ok(program) => program,
            Err(error) => {
                let error = PreparedVisualProgramBindError::Preparation(error);
                self.retain_author_snapshot_failure(
                    sequence.id,
                    sequence.revision,
                    registry_revision,
                    error.clone(),
                );
                return Err(error);
            }
        };
        let key = ScopedPreparedVisualProgramKey::new(self.scope_generation, program.key);
        if !self.entries.get(&key).is_some_and(|resident| Arc::ptr_eq(resident, &program)) {
            let error = PreparedVisualProgramBindError::ResidencyRejected {
                sequence_id: sequence.id,
                required_bytes: program.retained_bytes_estimate(),
                maximum_bytes: self.config.max_retained_bytes,
            };
            self.retain_author_snapshot_failure(
                sequence.id,
                sequence.revision,
                registry_revision,
                error.clone(),
            );
            return Err(error);
        }
        let binding =
            PreparedVisualProgramBinding::from_prepared(author_snapshot, sequence, program)?;
        self.author_snapshot_bindings.insert(
            sequence.id,
            AuthorSnapshotProgramBinding { key, binding: binding.clone() },
        );
        Ok(binding)
    }

    /// Return the exact prepared program for current author and definition
    /// revisions. Preparation failure leaves all prior entries intact.
    pub fn prepare(
        &mut self,
        sequence: &Sequence,
    ) -> std::result::Result<Arc<PreparedVisualProgram>, PreparedVisualProgramError> {
        self.author_fingerprint_evaluations = self.author_fingerprint_evaluations.saturating_add(1);
        let visual_author_fingerprint = prepared_visual_author_fingerprint(sequence)?;
        let mut retries = 0;
        loop {
            match self.prepare_once(sequence, visual_author_fingerprint) {
                Err(PreparedVisualProgramError::EffectRegistryChanged { .. })
                    if retries + 1 < MAX_VISUAL_DEFINITION_BIND_RETRIES =>
                {
                    retries += 1;
                }
                result => return result,
            }
        }
    }

    fn prepare_once(
        &mut self,
        sequence: &Sequence,
        visual_author_fingerprint: [u8; 32],
    ) -> std::result::Result<Arc<PreparedVisualProgram>, PreparedVisualProgramError> {
        let program_key = PreparedVisualProgramKey::for_sequence_with_author_fingerprint(
            sequence,
            visual_author_fingerprint,
        );
        let key = ScopedPreparedVisualProgramKey::new(self.scope_generation, program_key);
        if let Some(program) = self.entries.get(&key).cloned() {
            self.hits = self.hits.saturating_add(1);
            self.touch(key);
            return Ok(program);
        }

        let previous = self
            .entries
            .iter()
            .find(|(candidate_key, candidate)| {
                candidate_key.scope_generation == self.scope_generation
                    && candidate.sequence_id() == sequence.id
                    && candidate.effect_registry_revision() == program_key.effect_registry_revision
            })
            .map(|(_, candidate)| candidate)
            .cloned();
        let program = Arc::new(PreparedVisualProgram::prepare_reusing_with_key(
            sequence,
            previous.as_deref(),
            &self.lut_cache,
            program_key,
        )?);
        let prepared_key = ScopedPreparedVisualProgramKey::new(self.scope_generation, program.key);
        self.misses = self.misses.saturating_add(1);
        self.evict_obsolete_revisions(prepared_key);
        let program_bytes = program.retained_bytes_estimate();
        if self.config.max_entries == 0 || program_bytes > self.config.max_retained_bytes {
            self.rejected_residency = self.rejected_residency.saturating_add(1);
            return Ok(program);
        }
        self.trim_for_incoming(program_bytes);
        self.entries.insert(prepared_key, Arc::clone(&program));
        self.recency.push_back(prepared_key);
        self.retained_bytes = self.retained_bytes.saturating_add(program_bytes);
        Ok(program)
    }

    /// Explicitly revalidate immutable external resources for one Sequence.
    ///
    /// Stale programs are evicted and rebuilt on the next request. Unreadable
    /// resources also evict the program before returning diagnostic evidence.
    pub fn revalidate_dependencies(
        &mut self,
        sequence_id: SequenceId,
    ) -> std::result::Result<bool, PreparedVisualProgramDependencyError> {
        let keys = self
            .entries
            .keys()
            .copied()
            .filter(|key| key.program.sequence_id == sequence_id)
            .collect::<Vec<_>>();
        let mut all_current = true;
        for key in keys {
            let Some(program) = self.entries.get(&key) else {
                continue;
            };
            let current = program.dependencies_are_current();
            match current {
                Ok(true) => {}
                Ok(false) => {
                    all_current = false;
                    self.remove(key);
                }
                Err(error) => {
                    self.remove(key);
                    return Err(error);
                }
            }
        }
        Ok(all_current)
    }

    /// Drop every revision for one Sequence.
    pub fn invalidate_sequence(&mut self, sequence_id: SequenceId) {
        self.author_snapshot_bindings.remove(&sequence_id);
        self.author_snapshot_failures.remove(&sequence_id);
        let keys = self
            .entries
            .keys()
            .copied()
            .filter(|key| key.program.sequence_id == sequence_id)
            .collect::<Vec<_>>();
        for key in keys {
            self.remove(key);
        }
    }

    /// Drop all retained programs while preserving cumulative evidence.
    pub fn clear(&mut self) {
        self.evictions = self.evictions.saturating_add(self.entries.len() as u64);
        self.entries.clear();
        self.recency.clear();
        self.retained_bytes = 0;
        self.lut_cache.clear();
        self.author_snapshot_bindings.clear();
        self.author_snapshot_failures.clear();
    }

    /// Return bounded residency and reuse evidence.
    pub fn diagnostics(&self) -> PreparedVisualProgramCacheDiagnostics {
        PreparedVisualProgramCacheDiagnostics {
            entries: self.entries.len(),
            max_entries: self.config.max_entries,
            retained_bytes: self.retained_bytes,
            max_retained_bytes: self.config.max_retained_bytes,
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
            rejected_residency: self.rejected_residency,
            scope_rotations: self.scope_rotations,
            author_snapshot_binding_hits: self.author_snapshot_binding_hits,
            author_snapshot_binding_misses: self.author_snapshot_binding_misses,
            author_snapshot_binding_rotations: self.author_snapshot_binding_rotations,
            author_fingerprint_evaluations: self.author_fingerprint_evaluations,
            author_snapshot_failure_hits: self.author_snapshot_failure_hits,
        }
    }

    fn activate_author_snapshot(&mut self, author_snapshot: PreparedVisualAuthorSnapshotIdentity) {
        if self.author_snapshot == Some(author_snapshot) {
            return;
        }
        if self.author_snapshot.is_some() {
            self.author_snapshot_binding_rotations =
                self.author_snapshot_binding_rotations.saturating_add(1);
        }
        self.author_snapshot = Some(author_snapshot);
        self.author_snapshot_bindings.clear();
        self.author_snapshot_failures.clear();
    }

    fn retain_author_snapshot_failure(
        &mut self,
        sequence_id: SequenceId,
        sequence_revision: SequenceRevision,
        effect_registry_revision: u64,
        error: PreparedVisualProgramBindError,
    ) {
        self.author_snapshot_failures.insert(
            sequence_id,
            AuthorSnapshotBindingFailure { sequence_revision, effect_registry_revision, error },
        );
    }

    fn touch(&mut self, key: ScopedPreparedVisualProgramKey) {
        self.recency.retain(|candidate| *candidate != key);
        self.recency.push_back(key);
    }

    fn remove(&mut self, key: ScopedPreparedVisualProgramKey) {
        self.author_snapshot_bindings.retain(|_, binding| binding.key != key);
        if let Some(program) = self.entries.remove(&key) {
            self.retained_bytes =
                self.retained_bytes.saturating_sub(program.retained_bytes_estimate());
            self.evictions = self.evictions.saturating_add(1);
        }
        self.recency.retain(|candidate| *candidate != key);
    }

    fn trim_for_incoming(&mut self, incoming_bytes: usize) {
        while self.entries.len() >= self.config.max_entries
            || self.retained_bytes.saturating_add(incoming_bytes) > self.config.max_retained_bytes
        {
            let Some(stale) = self.recency.front().copied() else {
                break;
            };
            self.remove(stale);
        }
    }

    fn trim_to_config(&mut self) {
        while self.entries.len() > self.config.max_entries
            || self.retained_bytes > self.config.max_retained_bytes
        {
            let Some(stale) = self.recency.front().copied() else {
                break;
            };
            self.remove(stale);
        }
    }

    fn evict_obsolete_revisions(&mut self, current: ScopedPreparedVisualProgramKey) {
        let stale = self
            .entries
            .keys()
            .copied()
            .filter(|candidate| {
                candidate.scope_generation == current.scope_generation
                    && candidate.program.sequence_id == current.program.sequence_id
                    && *candidate != current
            })
            .collect::<Vec<_>>();
        for key in stale {
            self.remove(key);
        }
    }
}

impl Default for PreparedVisualProgramCache {
    fn default() -> Self {
        Self::new(DEFAULT_PREPARED_VISUAL_PROGRAM_CACHE_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline_render_plan::evaluate_timeline_render_plan;
    use crate::{
        admit_timeline_render_plan_for_cpu_compositor, evaluate_prepared_visual_program,
        TimelineEvaluationRequest, TimelineRenderPlan, TimelineRenderPlanElement,
    };
    use mondrian_core::automation::{ParameterResourceReference, PropertyValue};
    use mondrian_core::{AssetId, Color, FramePosition, Rational, TimelineTimeRange};
    use mondrian_effects::{
        effect_registry_revision, register_effect_definition, EffectColorDomainContract,
        EffectDefinition, EffectDeterminism, EffectExecutionContract, EffectExecutionModes,
        EffectGraphTopology, EffectNode, EffectNodeExt, EffectResourceLifetime,
        EffectRoiPropagation, EffectStateModel, EffectTemporalInputExtent, EffectTemporalSpan,
        EffectType,
    };
    use mondrian_timeline::{Clip, Track, VideoTransition};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn tt(frame: i64, rate: Rational) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, rate)).expect("valid test time")
    }

    fn fp(sequence: &Sequence, frame: i64) -> FramePosition {
        FramePosition::new(frame, sequence.time_base())
    }

    fn prepare_stable(sequence: &Sequence) -> PreparedVisualProgram {
        for _ in 0..32 {
            match PreparedVisualProgram::prepare(sequence) {
                Ok(program) => return program,
                Err(PreparedVisualProgramError::EffectRegistryChanged { .. }) => {}
                Err(error) => panic!("visual preparation failed: {error}"),
            }
        }
        panic!("Effect registry did not stabilize during test")
    }

    fn with_stable_effect_registry<T>(mut operation: impl FnMut() -> Option<T>) -> T {
        for _ in 0..64 {
            let before = effect_registry_revision();
            let Some(result) = operation() else {
                continue;
            };
            if effect_registry_revision() == before {
                return result;
            }
        }
        panic!("Effect registry did not stabilize during cache test")
    }

    #[test]
    fn program_freezes_materialization_contract_with_author_fingerprint() {
        let mut sequence = Sequence::new("materialization contract");
        sequence.settings.resolution = Resolution { width: 4096, height: 1716 };
        sequence.settings.preview.resolution_scale = 0.375;
        sequence.settings.title_safe_margin = 0.1375;

        let program = prepare_stable(&sequence);
        let contract = program.materialization_contract();

        assert_eq!(contract.author_resolution(), sequence.settings.resolution);
        assert_eq!(
            contract.authored_preview_resolution_scale().to_bits(),
            sequence.settings.preview.resolution_scale.to_bits()
        );
        assert_eq!(
            contract.title_safe_margin().to_bits(),
            sequence.settings.title_safe_margin.to_bits()
        );

        let mut changed = sequence.clone();
        changed.settings.preview.resolution_scale = 0.5;
        assert_ne!(
            prepared_visual_author_fingerprint(&changed).expect("changed fingerprint"),
            program.visual_author_fingerprint(),
            "authored Preview scale is part of the exact Program identity"
        );
    }

    #[test]
    fn visual_author_fingerprint_prevents_same_revision_false_reuse() {
        let mut sequence = Sequence::new("fingerprinted");
        sequence.video_tracks.clear();
        sequence.video_tracks.push(Track::new_video("V1"));
        let original_fingerprint =
            prepared_visual_author_fingerprint(&sequence).expect("original fingerprint");
        assert_eq!(
            prepare_stable(&sequence).visual_author_fingerprint(),
            original_fingerprint
        );
        assert_eq!(
            prepared_visual_author_fingerprint(&sequence.clone()).expect("clone fingerprint"),
            original_fingerprint,
            "copy-on-write allocation identity must not enter semantic identity"
        );

        let mut changed = sequence.clone();
        changed.video_tracks[0].is_visible = false;
        let changed_fingerprint =
            prepared_visual_author_fingerprint(&changed).expect("changed fingerprint");
        assert_ne!(changed_fingerprint, original_fingerprint);
        assert_eq!(changed.revision, sequence.revision);

        let mut cache = PreparedVisualProgramCache::new(4);
        let original = cache.prepare(&sequence).expect("original Program");
        let replacement = cache.prepare(&changed).expect("changed Program");
        assert!(!Arc::ptr_eq(&original, &replacement));
        assert_eq!(cache.diagnostics().entries, 1);
    }

    #[test]
    fn selected_range_preflight_rejects_deserialized_invalid_basic_title() {
        let mut sequence = Sequence::new("invalid selected title");
        sequence.video_tracks.clear();
        let rate = sequence.time_base();
        let mut track = Track::new_video("V1");
        track
            .add_clip(
                Clip::new_basic_title(
                    "valid",
                    "Mondrian Test Face",
                    TimelineTime::ZERO,
                    tt(5, rate),
                )
                .expect("title"),
            )
            .expect("add title");
        sequence.video_tracks.push(track);

        let mut encoded = serde_json::to_value(&sequence).expect("serialize Sequence");
        encoded["video_tracks"][0]["clips"][0]["content"]["title"]["properties"]["properties"]
            [mondrian_core::BasicTitle::TEXT_PATH]["static_value"]["Text"] =
            serde_json::Value::String(
                "x".repeat(mondrian_core::BASIC_TITLE_MAX_TEXT_BYTES.saturating_add(1)),
            );
        let invalid: Sequence =
            serde_json::from_value(encoded).expect("deserialize invalid author");
        let program = prepare_stable(&invalid);

        let error = program
            .preflight_range_dependencies(TimelineTime::ZERO, tt(5, rate))
            .expect_err("selected invalid title must fail");
        assert!(error.to_string().contains("UTF-8 bytes"));
    }

    fn single_solid_sequence(effect: Option<EffectNode>) -> Sequence {
        let mut sequence = Sequence::new("prepared visual program");
        sequence.video_tracks.clear();
        let rate = sequence.time_base();
        let mut track = Track::new_video("V1");
        let mut clip =
            Clip::new_solid_color(AssetId::new(), Color::BLACK, tt(0, rate), tt(20, rate))
                .expect("solid Clip");
        if let Some(effect) = effect {
            clip.add_effect_node(effect);
        }
        track.add_clip(clip).expect("add solid Clip");
        sequence.video_tracks.push(track);
        sequence
    }

    fn cpu_float_contract() -> EffectExecutionContract {
        EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: EffectRoiPropagation::PixelLocal,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::LinearChain,
        }
    }

    fn identity_effect_with_contract(label: &str, contract: EffectExecutionContract) -> EffectNode {
        static NEXT_DEFINITION: AtomicU64 = AtomicU64::new(1);
        let suffix = NEXT_DEFINITION.fetch_add(1, Ordering::Relaxed);
        let effect_type = EffectType::Plugin(format!("test.visual.preflight.{label}.{suffix}"));
        register_effect_definition(
            EffectDefinition::new(
                effect_type.key(),
                "Visual preflight identity",
                Default::default(),
                EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(contract)
            .with_graph_builder(Arc::new(|_, _, _| Ok(()))),
        )
        .expect("register visual preflight definition");
        EffectNode::new(effect_type)
    }

    fn first_effect_signature(plan: &TimelineRenderPlan) -> u64 {
        match &plan.elements[0] {
            TimelineRenderPlanElement::SolidColor(solid) => solid.effect_graph.signature_hash(),
            other => panic!("expected solid plan, got {other:?}"),
        }
    }

    #[test]
    fn prepared_and_direct_paths_match_animated_effect_semantics() {
        let sequence =
            single_solid_sequence(Some(EffectNode::with_defaults(EffectType::GaussianBlur)));
        let program = prepare_stable(&sequence);

        for frame in [0, 1, 7, 19] {
            let request = TimelineEvaluationRequest::export(fp(&sequence, frame));
            let direct =
                evaluate_timeline_render_plan(&sequence, request).expect("direct evaluation");
            let prepared =
                evaluate_prepared_visual_program(&program, request).expect("prepared evaluation");
            assert_eq!(prepared.elements.len(), direct.elements.len());
            assert_eq!(
                first_effect_signature(&prepared),
                first_effect_signature(&direct),
                "frame={frame}"
            );
            assert_eq!(prepared.diagnostics, direct.diagnostics);
        }
    }

    #[test]
    fn dynamic_preflight_rejects_identity_shaped_state_temporal_and_backend_obligations() {
        let stateful = EffectExecutionContract {
            state_model: EffectStateModel::StatefulSequential,
            resource_lifetime: EffectResourceLifetime::ContinuitySession,
            ..cpu_float_contract()
        };
        let temporal = EffectExecutionContract {
            temporal_input: EffectTemporalInputExtent {
                past: EffectTemporalSpan::Finite(
                    TimelineTime::new(1, 1).expect("positive temporal extent"),
                ),
                future: EffectTemporalSpan::None,
            },
            ..cpu_float_contract()
        };
        let gpu_only = EffectExecutionContract {
            execution_modes: EffectExecutionModes::GPU_F32,
            ..cpu_float_contract()
        };

        for (label, contract) in [
            ("stateful", stateful),
            ("temporal", temporal),
            ("gpu_only", gpu_only),
        ] {
            let sequence =
                single_solid_sequence(Some(identity_effect_with_contract(label, contract)));
            let program = prepare_stable(&sequence);
            let plan = evaluate_prepared_visual_program(
                &program,
                TimelineEvaluationRequest::export(fp(&sequence, 0)),
            )
            .expect("dynamic identity graph evaluates");
            let TimelineRenderPlanElement::SolidColor(solid) = &plan.elements[0] else {
                panic!("expected solid plan");
            };
            assert!(
                solid.effect_graph.graph().is_identity(),
                "the regression requires an identity-shaped graph"
            );
            assert!(
                admit_timeline_render_plan_for_cpu_compositor(&plan).is_err(),
                "{label} execution obligations must fail before pixel execution"
            );
        }
    }

    #[test]
    fn dynamic_preflight_admits_legal_identity_passthrough() {
        let sequence = single_solid_sequence(None);
        let program = prepare_stable(&sequence);
        let plan = evaluate_prepared_visual_program(
            &program,
            TimelineEvaluationRequest::export(fp(&sequence, 0)),
        )
        .expect("identity plan");
        let admission =
            admit_timeline_render_plan_for_cpu_compositor(&plan).expect("legal identity admission");
        assert_eq!(admission.effect_graphs, 1);
        assert_eq!(
            admission.precision,
            crate::TimelineCpuCompositePrecision::Float32
        );
    }

    #[test]
    fn dynamic_preview_preflight_rejects_encoded_working_fallback() {
        let contract = EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_U8,
            ..cpu_float_contract()
        };
        let sequence = single_solid_sequence(Some(identity_effect_with_contract(
            "encoded_only",
            contract,
        )));
        let program = prepare_stable(&sequence);
        let plan = evaluate_prepared_visual_program(
            &program,
            TimelineEvaluationRequest::preview(fp(&sequence, 0), 1.0),
        )
        .expect("encoded-only Preview identity plan");
        assert!(matches!(
            admit_timeline_render_plan_for_cpu_compositor(&plan),
            Err(crate::TimelineCompositeError::FloatEffect {
                reason: mondrian_effects::EffectFloatExecutionError::ExecutionContract(
                    mondrian_effects::EffectExecutionAdmissionError::ExecutionModeNotAdmitted {
                        backend: mondrian_effects::EffectProcessingBackend::Cpu,
                        precision: mondrian_effects::EffectWorkingPrecision::Float32,
                        ..
                    }
                )
            })
        ));
    }

    #[test]
    fn dynamic_preflight_rejects_mixed_cpu_precisions_without_a_transfer_route() {
        let float_effect = identity_effect_with_contract("mixed_float", cpu_float_contract());
        let encoded_effect = identity_effect_with_contract(
            "mixed_encoded",
            EffectExecutionContract {
                execution_modes: EffectExecutionModes::CPU_U8,
                ..cpu_float_contract()
            },
        );
        let mut sequence = single_solid_sequence(Some(float_effect));
        let rate = sequence.time_base();
        let mut second_track = Track::new_video("V2");
        let mut second_clip =
            Clip::new_solid_color(AssetId::new(), Color::WHITE, tt(0, rate), tt(20, rate))
                .expect("second solid Clip");
        second_clip.add_effect_node(encoded_effect);
        second_track.add_clip(second_clip).expect("add second solid Clip");
        sequence.video_tracks.push(second_track);

        let program = prepare_stable(&sequence);
        let plan = evaluate_prepared_visual_program(
            &program,
            TimelineEvaluationRequest::export(fp(&sequence, 0)),
        )
        .expect("mixed precision identity plan");
        assert!(
            admit_timeline_render_plan_for_cpu_compositor(&plan).is_err(),
            "the transfer-free compositor must reject a graph set with no common exact mode"
        );
    }

    #[test]
    fn blocked_later_clip_does_not_disable_unrelated_preview_region() {
        let mut sequence = single_solid_sequence(None);
        let rate = sequence.time_base();
        let blocked_effect =
            EffectNode::new(EffectType::Plugin("missing.preview.effect".to_owned()));
        let mut blocked =
            Clip::new_solid_color(AssetId::new(), Color::WHITE, tt(20, rate), tt(20, rate))
                .expect("blocked Clip");
        let blocked_id = blocked.id;
        blocked.add_effect_node(blocked_effect);
        sequence.video_tracks[0].add_clip(blocked).expect("add blocked Clip");

        let program = prepare_stable(&sequence);
        assert_eq!(program.diagnostics().blocked_clips, 1);
        assert!(
            !program.dependency_refresh_required().expect("blocked Clip refresh evidence"),
            "missing definitions wait for a registry revision instead of file polling"
        );
        assert!(program.preflight().is_err());
        assert!(evaluate_prepared_visual_program(
            &program,
            TimelineEvaluationRequest::preview(fp(&sequence, 5), 1.0),
        )
        .is_ok());
        let error = evaluate_prepared_visual_program(
            &program,
            TimelineEvaluationRequest::preview(fp(&sequence, 25), 1.0),
        )
        .expect_err("active blocked Clip must fail closed");
        assert!(error.to_string().contains(&blocked_id.to_string()));
    }

    #[test]
    fn negative_preflight_time_is_not_silently_clamped_to_frame_zero() {
        let blocked_effect =
            EffectNode::new(EffectType::Plugin("missing.frame-zero.effect".to_owned()));
        let program = prepare_stable(&single_solid_sequence(Some(blocked_effect)));

        assert!(
            program.preflight_frame(-1).is_ok(),
            "negative Sequence time is an empty interval before the frame-zero Clip"
        );
        assert!(
            program.preflight_frame(0).is_err(),
            "the blocker must still be reached at its actual frame-zero placement"
        );
    }

    #[test]
    fn unavailable_transition_is_prepared_but_blocks_only_its_active_interval() {
        let mut sequence = Sequence::new("prepared Transition blocker");
        sequence.video_tracks.clear();
        let rate = sequence.time_base();
        let mut track = Track::new_video("V1");
        let left = Clip::new_solid_color(AssetId::new(), Color::BLACK, tt(0, rate), tt(10, rate))
            .expect("left Clip");
        let right = Clip::new_solid_color(AssetId::new(), Color::WHITE, tt(10, rate), tt(10, rate))
            .expect("right Clip");
        let (left_id, right_id) = (left.id, right.id);
        track.add_clip(left).expect("add left");
        track.add_clip(right).expect("add right");
        sequence.video_tracks.push(track);
        let mut transition = VideoTransition::cross_dissolve(
            left_id,
            right_id,
            TimelineTimeRange::new(tt(8, rate), tt(4, rate)).expect("Transition range"),
        );
        transition.transition_type = VideoTransitionType::Plugin {
            definition_id: "vendor.missing.transition".to_owned(),
        };
        let transition_id = transition.id;
        sequence.video_transitions.push(transition);

        let program = prepare_stable(&sequence);
        assert_eq!(program.diagnostics().prepared_transitions, 0);
        assert_eq!(program.diagnostics().blocked_transitions, 1);
        assert_eq!(
            program.transition_blockers()[0].transition_id,
            transition_id
        );
        assert!(program.preflight().is_err());
        assert!(program.preflight_frame(5).is_ok());

        let error = program
            .preflight_frame(9)
            .expect_err("reachable unavailable Transition must fail closed");
        assert!(error.to_string().contains(&transition_id.to_string()));
        assert!(evaluate_prepared_visual_program(
            &program,
            TimelineEvaluationRequest::preview(fp(&sequence, 5), 1.0),
        )
        .is_ok());
        let error = evaluate_prepared_visual_program(
            &program,
            TimelineEvaluationRequest::preview(fp(&sequence, 9), 1.0),
        )
        .expect_err("active unavailable Transition must fail closed");
        assert!(error.to_string().contains(&transition_id.to_string()));
    }

    #[test]
    fn range_dependencies_include_transition_endpoint_handles() {
        let mut sequence = Sequence::new("Transition dependency handles");
        sequence.video_tracks.clear();
        let rate = sequence.time_base();
        let left_asset = AssetId::new();
        let right_asset = AssetId::new();
        let mut track = Track::new_video("V1");
        let left = Clip::new(left_asset, tt(0, rate), tt(10, rate)).expect("left media Clip");
        let right = Clip::new(right_asset, tt(10, rate), tt(10, rate)).expect("right media Clip");
        let (left_id, right_id) = (left.id, right.id);
        track.add_clip(left).expect("add left");
        track.add_clip(right).expect("add right");
        sequence.video_tracks.push(track);
        let transition = VideoTransition::cross_dissolve(
            left_id,
            right_id,
            TimelineTimeRange::new(tt(8, rate), tt(4, rate)).expect("Transition range"),
        );
        let transition_id = transition.id;
        sequence.video_transitions.push(transition);

        let dependencies = prepare_stable(&sequence)
            .preflight_range_dependencies(tt(10, rate), tt(10, rate))
            .expect("preflight Transition instant");

        let mut expected = vec![left_asset, right_asset];
        expected.sort_unstable_by_key(|asset_id| asset_id.to_string());
        assert_eq!(dependencies.media_asset_ids, expected);
        assert_eq!(dependencies.transition_ids, vec![transition_id]);
    }

    #[test]
    fn range_dependencies_expand_nested_window_by_finite_temporal_extent() {
        let mut sequence = Sequence::new("nested temporal dependency");
        sequence.video_tracks.clear();
        let rate = sequence.time_base();
        let nested_id = SequenceId::new();
        let temporal = EffectExecutionContract {
            temporal_input: EffectTemporalInputExtent {
                past: EffectTemporalSpan::Finite(tt(2, rate)),
                future: EffectTemporalSpan::None,
            },
            ..cpu_float_contract()
        };
        let mut nested_clip = Clip::new_nested_sequence(
            nested_id,
            tt(0, rate),
            tt(20, rate),
            Some("nested".to_owned()),
        )
        .expect("nested Clip");
        nested_clip.add_effect_node(identity_effect_with_contract("nested_history", temporal));
        let mut track = Track::new_video("V1");
        track.add_clip(nested_clip).expect("add nested Clip");
        sequence.video_tracks.push(track);

        let dependencies = prepare_stable(&sequence)
            .preflight_range_dependencies(tt(5, rate), tt(5, rate))
            .expect("preflight nested temporal instant");

        assert_eq!(
            dependencies.nested_demands,
            vec![PreparedVisualNestedRangeDemand {
                sequence_id: nested_id,
                range: PreparedVisualNestedRange::Bounded { first: tt(3, rate), last: tt(5, rate) },
            }]
        );
    }

    #[test]
    fn author_snapshot_binding_fingerprints_once_per_generation() {
        let sequence = single_solid_sequence(None);
        let (first, reused, second, first_diagnostics, second_diagnostics) =
            with_stable_effect_registry(|| {
                let mut cache = PreparedVisualProgramCache::new(4);
                let first_snapshot = PreparedVisualAuthorSnapshotIdentity::new(7);
                let first = cache.bind_author_snapshot(first_snapshot, &sequence).ok()?;
                let reused = cache.bind_author_snapshot(first_snapshot, &sequence).ok()?;
                let first_diagnostics = cache.diagnostics();
                let second = cache
                    .bind_author_snapshot(PreparedVisualAuthorSnapshotIdentity::new(8), &sequence)
                    .ok()?;
                let second_diagnostics = cache.diagnostics();
                Some((first, reused, second, first_diagnostics, second_diagnostics))
            });
        assert!(Arc::ptr_eq(first.program(), reused.program()));

        assert_eq!(first_diagnostics.author_fingerprint_evaluations, 1);
        assert_eq!(first_diagnostics.author_snapshot_binding_misses, 1);
        assert_eq!(first_diagnostics.author_snapshot_binding_hits, 1);
        assert_eq!(first_diagnostics.rejected_residency, 0);

        assert!(Arc::ptr_eq(first.program(), second.program()));
        assert_eq!(second_diagnostics.author_fingerprint_evaluations, 2);
        assert_eq!(second_diagnostics.author_snapshot_binding_rotations, 1);
    }

    #[test]
    fn author_snapshot_binding_negative_caches_residency_rejection() {
        let sequence = single_solid_sequence(None);
        let (first, second, diagnostics) = with_stable_effect_registry(|| {
            let mut cache = PreparedVisualProgramCache::with_config(
                PreparedVisualProgramCacheConfig::new(4, 0),
            );
            let snapshot = PreparedVisualAuthorSnapshotIdentity::new(11);
            let first = cache.bind_author_snapshot(snapshot, &sequence).err()?;
            let second = cache.bind_author_snapshot(snapshot, &sequence).err()?;
            Some((first, second, cache.diagnostics()))
        });
        assert_eq!(first, second);
        assert_eq!(diagnostics.author_fingerprint_evaluations, 1);
        assert_eq!(diagnostics.author_snapshot_failure_hits, 1);
        assert_eq!(diagnostics.rejected_residency, 1);
    }

    #[test]
    fn author_snapshot_residency_rejection_is_revalidated_after_reconfigure() {
        let sequence = single_solid_sequence(None);
        let (required_bytes, first_error, binding, diagnostics) =
            with_stable_effect_registry(|| {
                let required_bytes =
                    PreparedVisualProgram::prepare(&sequence).ok()?.retained_bytes_estimate();
                if required_bytes == 0 {
                    return None;
                }
                let snapshot = PreparedVisualAuthorSnapshotIdentity::new(12);
                let mut cache = PreparedVisualProgramCache::with_config(
                    PreparedVisualProgramCacheConfig::new(4, required_bytes.saturating_sub(1)),
                );

                let first_error = cache.bind_author_snapshot(snapshot, &sequence).err()?;
                cache.reconfigure(PreparedVisualProgramCacheConfig::new(4, required_bytes));
                let binding = cache.bind_author_snapshot(snapshot, &sequence).ok()?;

                Some((required_bytes, first_error, binding, cache.diagnostics()))
            });

        assert_eq!(
            first_error,
            PreparedVisualProgramBindError::ResidencyRejected {
                sequence_id: sequence.id,
                required_bytes,
                maximum_bytes: required_bytes.saturating_sub(1),
            }
        );
        assert_eq!(
            binding.author_snapshot(),
            Some(PreparedVisualAuthorSnapshotIdentity::new(12))
        );
        assert_eq!(diagnostics.entries, 1);
        assert_eq!(diagnostics.retained_bytes, required_bytes);
        assert_eq!(diagnostics.author_snapshot_binding_misses, 2);
        assert_eq!(diagnostics.author_fingerprint_evaluations, 2);
        assert_eq!(diagnostics.author_snapshot_failure_hits, 0);
        assert_eq!(diagnostics.rejected_residency, 1);
    }

    #[test]
    fn modeled_keyers_remain_reachable_fail_closed_effects() {
        for effect_type in [EffectType::ChromaKey, EffectType::LumaKey] {
            let sequence =
                single_solid_sequence(Some(EffectNode::with_defaults(effect_type.clone())));
            let program = prepare_stable(&sequence);
            let error = evaluate_prepared_visual_program(
                &program,
                TimelineEvaluationRequest::export(fp(&sequence, 0)),
            )
            .expect_err("modeled-only keyer must not produce plausible pixels");
            assert!(
                error.to_string().contains(&effect_type.key()),
                "blocker should identify the unavailable definition: {error}"
            );
        }
    }

    #[test]
    fn cache_reuses_exact_revision_and_evicts_obsolete_program() {
        let (first, reused, second, diagnostics) = with_stable_effect_registry(|| {
            let mut sequence = single_solid_sequence(None);
            let mut cache = PreparedVisualProgramCache::new(4);
            let first = match cache.prepare(&sequence) {
                Ok(program) => program,
                Err(PreparedVisualProgramError::EffectRegistryChanged { .. }) => return None,
                Err(error) => panic!("first program failed: {error}"),
            };
            let reused = match cache.prepare(&sequence) {
                Ok(program) => program,
                Err(PreparedVisualProgramError::EffectRegistryChanged { .. }) => return None,
                Err(error) => panic!("reused program failed: {error}"),
            };

            sequence.revision = sequence.revision.checked_next().expect("next revision");
            let second = match cache.prepare(&sequence) {
                Ok(program) => program,
                Err(PreparedVisualProgramError::EffectRegistryChanged { .. }) => return None,
                Err(error) => panic!("replacement program failed: {error}"),
            };
            Some((first, reused, second, cache.diagnostics()))
        });
        assert!(Arc::ptr_eq(&first, &reused));
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(diagnostics.entries, 1);
        assert_eq!(diagnostics.hits, 1);
        assert_eq!(diagnostics.misses, 2);
        assert_eq!(diagnostics.evictions, 1);
    }

    #[test]
    fn cache_rejects_oversized_residency_and_online_reconfigure_admits_next_build() {
        let sequence = single_solid_sequence(None);
        let reference = prepare_stable(&sequence);
        let required_bytes = reference.retained_bytes_estimate();
        assert!(required_bytes > 0);
        let mut cache = PreparedVisualProgramCache::with_config(
            PreparedVisualProgramCacheConfig::new(4, required_bytes.saturating_sub(1)),
        );

        let unretained = cache.prepare(&sequence).expect("oversized program remains usable");
        assert_eq!(unretained.retained_bytes_estimate(), required_bytes);
        assert_eq!(cache.diagnostics().entries, 0);
        assert_eq!(cache.diagnostics().retained_bytes, 0);
        assert_eq!(cache.diagnostics().rejected_residency, 1);

        cache.reconfigure(PreparedVisualProgramCacheConfig::new(4, required_bytes));
        let retained = cache.prepare(&sequence).expect("program fits updated grant");
        assert_eq!(retained.retained_bytes_estimate(), required_bytes);
        assert_eq!(cache.diagnostics().entries, 1);
        assert_eq!(cache.diagnostics().retained_bytes, required_bytes);
    }

    #[test]
    fn cache_scope_rotation_rejects_same_ids_and_revision_from_another_author_lifetime() {
        fn prepare_once(
            cache: &mut PreparedVisualProgramCache,
            sequence: &Sequence,
        ) -> Option<Arc<PreparedVisualProgram>> {
            match cache.prepare(sequence) {
                Ok(program) => Some(program),
                Err(PreparedVisualProgramError::EffectRegistryChanged { .. }) => None,
                Err(error) => panic!("unexpected visual preparation failure: {error}"),
            }
        }

        let first_sequence = single_solid_sequence(None);
        let mut second_sequence = first_sequence.clone();
        let ClipContent::SolidColor { color, .. } =
            &mut second_sequence.video_tracks[0].clips[0].content
        else {
            panic!("test fixture must remain a Solid Color Clip");
        };
        *color = Color::WHITE;

        assert_eq!(first_sequence.id, second_sequence.id);
        assert_eq!(first_sequence.revision, second_sequence.revision);
        assert_eq!(
            first_sequence.video_tracks[0].id,
            second_sequence.video_tracks[0].id
        );
        assert_eq!(
            first_sequence.video_tracks[0].clips[0].id,
            second_sequence.video_tracks[0].clips[0].id
        );

        let mut cache = PreparedVisualProgramCache::new(4);
        let first = (0..64)
            .find_map(|_| {
                let first = prepare_once(&mut cache, &first_sequence)?;
                let exact_reuse = prepare_once(&mut cache, &first_sequence)?;
                Arc::ptr_eq(&first, &exact_reuse).then_some(first)
            })
            .expect("Effect registry must stabilize for same-scope reuse");

        cache.rotate_scope();
        let second = (0..64)
            .find_map(|_| prepare_once(&mut cache, &second_sequence))
            .expect("Effect registry must stabilize for second author lifetime");
        assert!(
            !Arc::ptr_eq(&first, &second),
            "durable IDs and revisions cannot authorize cross-session reuse"
        );
        let plan = evaluate_prepared_visual_program(
            &second,
            TimelineEvaluationRequest::export(fp(&second_sequence, 0)),
        )
        .expect("second author plan");
        let TimelineRenderPlanElement::SolidColor(solid) = &plan.elements[0] else {
            panic!("second author plan must remain a Solid Color");
        };
        assert_eq!(solid.color, Color::WHITE);

        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 1);
        assert!(diagnostics.hits >= 1);
        assert!(diagnostics.misses >= 2);
        assert!(diagnostics.evictions >= 1);
        assert_eq!(diagnostics.scope_rotations, 1);
    }

    #[test]
    fn new_sequence_revision_reuses_only_exact_clip_effect_author_fingerprints() {
        let mut sequence =
            single_solid_sequence(Some(EffectNode::with_defaults(EffectType::GaussianBlur)));
        let rate = sequence.time_base();
        sequence.video_tracks[0]
            .add_clip(
                Clip::new_solid_color(AssetId::new(), Color::WHITE, tt(30, rate), tt(10, rate))
                    .expect("second Clip"),
            )
            .expect("add second Clip");
        let mut cache = PreparedVisualProgramCache::new(4);
        cache.prepare(&sequence).expect("initial program");

        sequence.video_tracks[0].clips[0].position = tt(1, rate);
        sequence.revision = sequence.revision.checked_next().expect("placement revision");
        let placement_only = cache.prepare(&sequence).expect("placement-only replacement");
        assert_eq!(placement_only.diagnostics().prepared_clips, 2);
        assert_eq!(placement_only.diagnostics().reused_clips, 2);

        sequence.video_tracks[0].clips[0].effects[0].is_enabled = false;
        sequence.revision = sequence.revision.checked_next().expect("Effect revision");
        let effect_changed = cache.prepare(&sequence).expect("Effect replacement");
        assert_eq!(effect_changed.diagnostics().prepared_clips, 2);
        assert_eq!(effect_changed.diagnostics().reused_clips, 1);
    }

    #[test]
    fn dependency_refresh_retries_external_change_but_not_author_edit() {
        let author_edit_refresh = with_stable_effect_registry(|| {
            let author_edit = prepare_stable(&single_solid_sequence(Some(
                EffectNode::with_defaults(EffectType::Lut3D),
            )));
            assert_eq!(author_edit.diagnostics().blocked_clips, 1);
            Some(author_edit.dependency_refresh_required().expect("author-edit blocker evidence"))
        });
        assert!(!author_edit_refresh);

        let mut missing_file = EffectNode::with_defaults(EffectType::Lut3D);
        let path_id = EffectType::Lut3D.parameter_id("path").expect("path parameter ID");
        let processing_space_id = EffectType::Lut3D
            .parameter_id("processing_space")
            .expect("processing-space parameter ID");
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let missing_path = std::env::temp_dir().join(format!("mondrian-missing-lut-{unique}.cube"));
        missing_file
            .set_static_value_by_parameter(
                &processing_space_id,
                PropertyValue::Enum("scene_linear".to_owned()),
            )
            .expect("set LUT processing space");
        missing_file
            .set_static_value_by_parameter(
                &path_id,
                PropertyValue::Resource(ParameterResourceReference::ExternalFile {
                    path: missing_path,
                }),
            )
            .expect("set missing LUT path");
        let external_change = prepare_stable(&single_solid_sequence(Some(missing_file)));
        assert_eq!(external_change.diagnostics().blocked_clips, 1);
        assert!(external_change
            .dependency_refresh_required()
            .expect("external-change blocker evidence"));
    }

    #[test]
    fn new_sequence_revision_reprepares_external_resource_effects() {
        let reused_clips = with_stable_effect_registry(|| {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let path = std::env::temp_dir().join(format!("mondrian-visual-program-{unique}.cube"));
            std::fs::write(
                &path,
                "LUT_3D_SIZE 2
0 0 0
1 0 0
0 1 0
1 1 0
0 0 1
1 0 1
0 1 1
1 1 1
",
            )
            .expect("write LUT");

            let mut effect = EffectNode::with_defaults(EffectType::Lut3D);
            let path_id = EffectType::Lut3D.parameter_id("path").expect("path parameter ID");
            let processing_space_id = EffectType::Lut3D
                .parameter_id("processing_space")
                .expect("processing-space parameter ID");
            effect
                .set_static_value_by_parameter(
                    &processing_space_id,
                    PropertyValue::Enum("scene_linear".to_owned()),
                )
                .expect("set LUT processing space");
            effect
                .set_static_value_by_parameter(
                    &path_id,
                    PropertyValue::Resource(ParameterResourceReference::ExternalFile {
                        path: path.clone(),
                    }),
                )
                .expect("set LUT path");
            let mut sequence = single_solid_sequence(Some(effect));
            let mut cache = PreparedVisualProgramCache::new(4);
            let first = match cache.prepare(&sequence) {
                Ok(program) => program,
                Err(PreparedVisualProgramError::EffectRegistryChanged { .. }) => {
                    std::fs::remove_file(path).expect("remove superseded LUT");
                    return None;
                }
                Err(error) => panic!("initial resource program failed: {error}"),
            };
            assert_eq!(first.diagnostics().blocked_clips, 0);
            let refresh_required = first.dependency_refresh_required().expect("current dependency");
            if effect_registry_revision() != first.effect_registry_revision() {
                std::fs::remove_file(path).expect("remove superseded LUT");
                return None;
            }
            assert!(!refresh_required);

            sequence.video_tracks[0].clips[0].position = tt(1, sequence.time_base());
            sequence.revision = sequence.revision.checked_next().expect("placement revision");
            let replacement = match cache.prepare(&sequence) {
                Ok(program) => program,
                Err(PreparedVisualProgramError::EffectRegistryChanged { .. }) => {
                    std::fs::remove_file(path).expect("remove superseded LUT");
                    return None;
                }
                Err(error) => panic!("replacement resource program failed: {error}"),
            };
            let reused_clips = replacement.diagnostics().reused_clips;

            std::fs::remove_file(path).expect("remove LUT");
            Some(reused_clips)
        });
        assert_eq!(reused_clips, 0);
    }

    #[test]
    fn failed_replacement_leaves_last_valid_program_resident() {
        let (replacement, diagnostics, first_revision) = with_stable_effect_registry(|| {
            let mut sequence = single_solid_sequence(None);
            let mut cache = PreparedVisualProgramCache::new(4);
            let first = match cache.prepare(&sequence) {
                Ok(program) => program,
                Err(PreparedVisualProgramError::EffectRegistryChanged { .. }) => return None,
                Err(error) => panic!("first program failed: {error}"),
            };

            sequence.revision = sequence.revision.checked_next().expect("next revision");
            let duplicate = sequence.video_tracks[0].clips[0].clone();
            sequence.video_tracks[0].clips.push(duplicate);
            let replacement = cache.prepare(&sequence);
            Some((replacement, cache.diagnostics(), first.sequence_revision()))
        });
        assert!(matches!(
            replacement,
            Err(PreparedVisualProgramError::Schedule { ref reason, .. })
                if reason.contains("occurs more than once")
        ));
        assert_eq!(diagnostics.entries, 1);
        assert_eq!(diagnostics.misses, 1);
        assert_eq!(diagnostics.evictions, 0);
        assert_eq!(first_revision, SequenceRevision::INITIAL);
    }

    #[test]
    fn explicit_clear_records_every_retired_program() {
        let sequence = single_solid_sequence(None);
        let mut cache = PreparedVisualProgramCache::new(4);
        cache.prepare(&sequence).expect("resident program");

        cache.clear();

        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 0);
        assert_eq!(diagnostics.evictions, 1);
    }
}
