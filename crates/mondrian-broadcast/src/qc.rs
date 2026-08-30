use mondrian_core::{ColorSpace, SignalComplianceContract};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const QC_REPORT_SCHEMA_VERSION: u32 = 1;
const MAX_PROFILE_RULES: usize = 16;
const PARTS_PER_MILLION: u64 = 1_000_000;

/// Rectangular active-picture area evaluated by every QC rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QcActivePicture {
    /// Full raster width.
    pub raster_width: u32,
    /// Full raster height.
    pub raster_height: u32,
    /// Active-area left coordinate.
    pub x: u32,
    /// Active-area top coordinate.
    pub y: u32,
    /// Active-area width.
    pub width: u32,
    /// Active-area height.
    pub height: u32,
}

impl QcActivePicture {
    /// Select the complete raster as active picture.
    pub const fn full(width: u32, height: u32) -> Self {
        Self {
            raster_width: width,
            raster_height: height,
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    fn validate(self) -> Result<Self, BroadcastQcError> {
        let right = self.x.checked_add(self.width).ok_or(BroadcastQcError::ExtentOverflow)?;
        let bottom = self.y.checked_add(self.height).ok_or(BroadcastQcError::ExtentOverflow)?;
        if self.raster_width == 0
            || self.raster_height == 0
            || self.width == 0
            || self.height == 0
            || right > self.raster_width
            || bottom > self.raster_height
        {
            return Err(BroadcastQcError::InvalidActivePicture);
        }
        Ok(self)
    }

    fn raster_pixels(self) -> Result<usize, BroadcastQcError> {
        usize::try_from(self.raster_width)
            .ok()
            .and_then(|width| width.checked_mul(self.raster_height as usize))
            .ok_or(BroadcastQcError::ExtentOverflow)
    }

    fn active_pixels(self) -> Result<u64, BroadcastQcError> {
        u64::from(self.width)
            .checked_mul(u64::from(self.height))
            .ok_or(BroadcastQcError::ExtentOverflow)
    }
}

/// Delivery-profile severity assigned to one rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BroadcastQcSeverity {
    /// Evidence is retained but does not affect the verdict.
    Info,
    /// Evidence makes the result a warning.
    Warn,
    /// Evidence blocks a strict delivery publication gate.
    Fail,
}

/// Exact image boundary observed by one QC profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BroadcastQcObservationTap {
    /// Encoded-float Program Output after output transform and before Legalizer.
    ProgramSignalBeforeLegalizer,
    /// Delivery picture after explicit Legalizer and output quantization readback.
    #[default]
    DeliveryPictureAfterLegalizer,
}

/// One exact versioned content-analysis rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BroadcastQcRule {
    /// Detect sustained frames whose active picture is predominantly black.
    Black {
        /// Stable profile-local rule identity.
        rule_id: String,
        /// Maximum encoded luma classified as black.
        maximum_encoded_luma: f32,
        /// Minimum active pixels classified as black, in parts per million.
        minimum_coverage_ppm: u32,
        /// Minimum inclusive segment duration.
        minimum_frames: u32,
        /// Profile outcome for a finding.
        severity: BroadcastQcSeverity,
    },
    /// Detect sustained visually unchanged active pictures.
    Freeze {
        /// Stable profile-local rule identity.
        rule_id: String,
        /// A pixel changes when any encoded RGB channel exceeds this delta.
        maximum_channel_delta: f32,
        /// Maximum changed-pixel coverage still classified as frozen.
        maximum_changed_ppm: u32,
        /// Minimum inclusive segment duration, including the baseline frame.
        minimum_frames: u32,
        /// Profile outcome for a finding.
        severity: BroadcastQcSeverity,
    },
    /// Detect adjacent-frame luma transitions for operator review.
    ///
    /// This is deliberately not an ITU-R BT.1702/PSE certification algorithm.
    LumaFlashCandidate {
        /// Stable profile-local rule identity.
        rule_id: String,
        /// Minimum absolute active-picture mean-luma delta.
        minimum_mean_luma_delta: f32,
        /// Profile outcome for a candidate.
        severity: BroadcastQcSeverity,
    },
    /// Detect RGB samples beyond the selected signal cube and tolerance.
    SignalExcursion {
        /// Stable profile-local rule identity.
        rule_id: String,
        /// Symmetric tolerance beyond nominal zero through one.
        tolerance_per_mille: u16,
        /// Maximum tolerated out-of-range coverage.
        maximum_coverage_ppm: u32,
        /// Profile outcome for a finding.
        severity: BroadcastQcSeverity,
    },
}

impl BroadcastQcRule {
    /// Stable rule identity inside one profile edition.
    pub fn rule_id(&self) -> &str {
        match self {
            Self::Black { rule_id, .. }
            | Self::Freeze { rule_id, .. }
            | Self::LumaFlashCandidate { rule_id, .. }
            | Self::SignalExcursion { rule_id, .. } => rule_id,
        }
    }

    /// Delivery-profile severity.
    pub const fn severity(&self) -> BroadcastQcSeverity {
        match self {
            Self::Black { severity, .. }
            | Self::Freeze { severity, .. }
            | Self::LumaFlashCandidate { severity, .. }
            | Self::SignalExcursion { severity, .. } => *severity,
        }
    }

    fn validate(&self) -> Result<(), BroadcastQcError> {
        if self.rule_id().trim().is_empty() {
            return Err(BroadcastQcError::BlankRuleId);
        }
        match self {
            Self::Black {
                maximum_encoded_luma,
                minimum_coverage_ppm,
                minimum_frames,
                ..
            } => {
                validate_unit_float(*maximum_encoded_luma)?;
                validate_ppm(*minimum_coverage_ppm)?;
                validate_duration(*minimum_frames)
            }
            Self::Freeze {
                maximum_channel_delta,
                maximum_changed_ppm,
                minimum_frames,
                ..
            } => {
                validate_unit_float(*maximum_channel_delta)?;
                validate_ppm(*maximum_changed_ppm)?;
                validate_duration(*minimum_frames)
            }
            Self::LumaFlashCandidate { minimum_mean_luma_delta, .. } => {
                validate_unit_float(*minimum_mean_luma_delta)
            }
            Self::SignalExcursion { tolerance_per_mille, maximum_coverage_ppm, .. } => {
                if *tolerance_per_mille > 1_000 {
                    return Err(BroadcastQcError::InvalidTolerance);
                }
                validate_ppm(*maximum_coverage_ppm)
            }
        }
    }
}

/// Frozen broadcaster/delivery QC contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BroadcastQcProfile {
    /// Stable profile name, such as a broadcaster shim identifier.
    pub id: String,
    /// Exact profile edition.
    pub edition: String,
    /// SHA-256 of the external specification or approved profile source.
    pub source_sha256: [u8; 32],
    /// Display-encoded Program Output signal identity.
    pub signal_color_space: ColorSpace,
    /// Exact Program Output/delivery boundary analyzed by this profile.
    #[serde(default)]
    pub observation_tap: BroadcastQcObservationTap,
    /// Explicit active picture; letterbox/pillarbox policy is therefore frozen.
    pub active_picture: QcActivePicture,
    /// Ordered exact rules.
    pub rules: Vec<BroadcastQcRule>,
    /// Maximum retained findings; overflow makes the report incomplete.
    pub maximum_retained_findings: u32,
    /// Require a separately qualified regulatory photosensitive-flash result.
    pub require_regulatory_flash_analysis: bool,
    /// Require independent decode-and-rescan evidence for the encoded artifact.
    #[serde(default)]
    pub require_encoded_artifact_revalidation: bool,
}

impl BroadcastQcProfile {
    /// Validate all profile identity, raster, color, thresholds, and bounds.
    pub fn validate(&self) -> Result<(), BroadcastQcError> {
        if self.id.trim().is_empty() || self.edition.trim().is_empty() {
            return Err(BroadcastQcError::BlankProfileIdentity);
        }
        self.active_picture.validate()?;
        SignalComplianceContract::normalized_rgb(self.signal_color_space)?;
        if self.rules.is_empty() || self.rules.len() > MAX_PROFILE_RULES {
            return Err(BroadcastQcError::InvalidRuleCount { actual: self.rules.len() });
        }
        if self.maximum_retained_findings == 0 {
            return Err(BroadcastQcError::ZeroFindingCapacity);
        }
        for rule in &self.rules {
            rule.validate()?;
        }
        for (index, rule) in self.rules.iter().enumerate() {
            if self.rules[index + 1..]
                .iter()
                .any(|candidate| candidate.rule_id() == rule.rule_id())
            {
                return Err(BroadcastQcError::DuplicateRuleId {
                    rule_id: rule.rule_id().to_owned(),
                });
            }
        }
        Ok(())
    }

    /// Deterministic profile identity used by reports and export snapshots.
    pub fn fingerprint(&self) -> Result<[u8; 32], BroadcastQcError> {
        self.validate()?;
        let mut hasher = Sha256::new();
        update_profile_hash(&mut hasher, self);
        Ok(hasher.finalize().into())
    }
}

/// Borrowed encoded-float Program Output frame.
#[derive(Debug, Clone, Copy)]
pub struct BroadcastQcFrame<'a> {
    /// Absolute non-wrapping frame coordinate.
    pub frame_index: u64,
    /// Straight RGBA pixels at the profile's explicit observation tap.
    pub rgba: &'a [[f32; 4]],
}

/// Stable content-finding family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BroadcastQcFindingKind {
    /// Sustained black segment.
    BlackSegment,
    /// Sustained unchanged segment.
    FreezeSegment,
    /// Adjacent-frame luma transition for review.
    LumaFlashCandidate,
    /// Sustained RGB signal excursion.
    SignalExcursion,
}

/// One deterministic frame-addressed QC finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BroadcastQcFinding {
    /// Stable profile-local rule identity.
    pub rule_id: String,
    /// Finding family.
    pub kind: BroadcastQcFindingKind,
    /// Profile severity.
    pub severity: BroadcastQcSeverity,
    /// Inclusive absolute first frame.
    pub start_frame: u64,
    /// Inclusive absolute last frame.
    pub end_frame: u64,
    /// Peak measured coverage in parts per million, when applicable.
    pub measured_coverage_ppm: Option<u32>,
    /// Peak mean-luma delta in millionths, when applicable.
    pub measured_luma_delta_millionths: Option<u32>,
}

/// Outcome of one rule or external obligation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BroadcastQcRuleStatus {
    /// Fully evaluated with no findings.
    Pass,
    /// Fully evaluated with one or more findings.
    Fail,
    /// Required external analysis did not run.
    NotTested,
    /// Input/evidence was incomplete.
    Inconclusive,
    /// The selected software Implementation does not support the requirement.
    Unsupported,
}

/// External qualification obligation that cannot be inferred from candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BroadcastQcObligationKind {
    /// Current ITU-R BT.1702 or broadcaster-approved PSE analysis.
    RegulatoryPhotosensitiveFlash,
    /// Independent decode and content re-scan of the published encoded artifact.
    EncodedArtifactRevalidation,
}

/// Explicit unresolved external obligation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BroadcastQcObligation {
    /// Required external analysis.
    pub kind: BroadcastQcObligationKind,
    /// Current evidence state.
    pub status: BroadcastQcRuleStatus,
    /// Stable operator-facing explanation.
    pub detail: String,
}

/// Overall report verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BroadcastQcVerdict {
    /// Every required software rule passed and no external obligation remained.
    Pass,
    /// Software analysis completed with warning/info findings or obligations.
    Warn,
    /// At least one profile-fatal finding occurred.
    Fail,
    /// Scan coverage or retained evidence was incomplete.
    Incomplete,
}

/// Versioned, bounded, deterministic broadcast QC evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BroadcastQcReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Profile identity.
    pub profile_id: String,
    /// Profile edition.
    pub profile_edition: String,
    /// Frozen profile fingerprint.
    pub profile_fingerprint: [u8; 32],
    /// Overall verdict.
    pub verdict: BroadcastQcVerdict,
    /// Whether the caller declared an uninterrupted complete scan.
    pub complete: bool,
    /// Number of analyzed contiguous frames.
    pub analyzed_frames: u64,
    /// First analyzed frame, if any.
    pub first_frame: Option<u64>,
    /// Last analyzed frame, if any.
    pub last_frame: Option<u64>,
    /// Total findings including overflow.
    pub finding_count: u64,
    /// Findings retained in deterministic order.
    pub findings: Vec<BroadcastQcFinding>,
    /// Findings omitted after the explicit bound was reached.
    pub overflow_count: u64,
    /// Required external obligations.
    pub obligations: Vec<BroadcastQcObligation>,
    /// Digest over profile identity and all report evidence.
    pub evidence_sha256: [u8; 32],
}

impl BroadcastQcReport {
    /// Verify that the retained evidence still matches its canonical digest.
    pub fn verify_evidence(&self) -> bool {
        self.evidence_sha256 == report_digest(self)
    }
}

/// Streaming O(frame pixels + bounded findings) QC execution.
pub struct BroadcastQcSession {
    profile: BroadcastQcProfile,
    profile_fingerprint: [u8; 32],
    contract: SignalComplianceContract,
    previous_frame: Option<Vec<[f32; 4]>>,
    previous_mean_luma: Option<f32>,
    first_frame: Option<u64>,
    last_frame: Option<u64>,
    analyzed_frames: u64,
    findings: Vec<BroadcastQcFinding>,
    finding_count: u64,
    overflow_count: u64,
    black_runs: Vec<RunState>,
    freeze_runs: Vec<RunState>,
    excursion_runs: Vec<RunState>,
    invalid_input: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct RunState {
    start: Option<u64>,
    end: u64,
    peak_coverage_ppm: u32,
}

impl BroadcastQcSession {
    /// Prepare one bounded analyzer from an immutable profile.
    pub fn new(profile: BroadcastQcProfile) -> Result<Self, BroadcastQcError> {
        profile.validate()?;
        let profile_fingerprint = profile.fingerprint()?;
        let contract = SignalComplianceContract::normalized_rgb(profile.signal_color_space)?;
        let black_runs = vec![RunState::default(); profile.rules.len()];
        let freeze_runs = black_runs.clone();
        let excursion_runs = black_runs.clone();
        Ok(Self {
            profile,
            profile_fingerprint,
            contract,
            previous_frame: None,
            previous_mean_luma: None,
            first_frame: None,
            last_frame: None,
            analyzed_frames: 0,
            findings: Vec::new(),
            finding_count: 0,
            overflow_count: 0,
            black_runs,
            freeze_runs,
            excursion_runs,
            invalid_input: false,
        })
    }

    /// Analyze one exact next frame. Gaps and non-finite samples fail closed.
    pub fn push(&mut self, frame: BroadcastQcFrame<'_>) -> Result<(), BroadcastQcError> {
        if let Some(previous) = self.last_frame {
            let expected = previous.checked_add(1).ok_or(BroadcastQcError::FrameIndexOverflow)?;
            if frame.frame_index != expected {
                self.invalid_input = true;
                return Err(BroadcastQcError::NonContiguousFrame {
                    expected,
                    actual: frame.frame_index,
                });
            }
        }
        let expected_pixels = self.profile.active_picture.raster_pixels()?;
        if frame.rgba.len() != expected_pixels {
            self.invalid_input = true;
            return Err(BroadcastQcError::RasterLength {
                expected: expected_pixels,
                actual: frame.rgba.len(),
            });
        }
        let metrics = match measure_frame(
            frame.rgba,
            self.previous_frame.as_deref(),
            self.profile.active_picture,
            self.contract,
            &self.profile.rules,
        ) {
            Ok(metrics) => metrics,
            Err(error) => {
                self.invalid_input = true;
                return Err(error);
            }
        };
        if self.first_frame.is_none() {
            self.first_frame = Some(frame.frame_index);
        }
        let rules = self.profile.rules.clone();
        for (index, rule) in rules.iter().enumerate() {
            match rule {
                BroadcastQcRule::Black { minimum_coverage_ppm, minimum_frames, .. } => self
                    .update_run(
                        index,
                        RunKind::Black,
                        metrics.rule_coverage_ppm[index] >= *minimum_coverage_ppm,
                        frame.frame_index,
                        metrics.rule_coverage_ppm[index],
                        *minimum_frames,
                        rule,
                    )?,
                BroadcastQcRule::Freeze { maximum_changed_ppm, minimum_frames, .. } => {
                    let frozen = self.previous_frame.is_some()
                        && metrics.rule_coverage_ppm[index] <= *maximum_changed_ppm;
                    self.update_run(
                        index,
                        RunKind::Freeze,
                        frozen,
                        frame.frame_index,
                        metrics.rule_coverage_ppm[index],
                        *minimum_frames,
                        rule,
                    )?;
                }
                BroadcastQcRule::LumaFlashCandidate { minimum_mean_luma_delta, .. } => {
                    if let Some(previous) = self.previous_mean_luma {
                        let delta = (metrics.mean_luma - previous).abs();
                        if delta >= *minimum_mean_luma_delta {
                            self.retain_finding(BroadcastQcFinding {
                                rule_id: rule.rule_id().to_owned(),
                                kind: BroadcastQcFindingKind::LumaFlashCandidate,
                                severity: rule.severity(),
                                start_frame: frame.frame_index.saturating_sub(1),
                                end_frame: frame.frame_index,
                                measured_coverage_ppm: None,
                                measured_luma_delta_millionths: Some(unit_to_millionths(delta)),
                            });
                        }
                    }
                }
                BroadcastQcRule::SignalExcursion { maximum_coverage_ppm, .. } => {
                    self.update_run(
                        index,
                        RunKind::Excursion,
                        metrics.rule_coverage_ppm[index] > *maximum_coverage_ppm,
                        frame.frame_index,
                        metrics.rule_coverage_ppm[index],
                        1,
                        rule,
                    )?;
                }
            }
        }
        self.previous_mean_luma = Some(metrics.mean_luma);
        self.previous_frame = Some(frame.rgba.to_vec());
        self.last_frame = Some(frame.frame_index);
        self.analyzed_frames = self.analyzed_frames.saturating_add(1);
        Ok(())
    }

    /// Flush open segments and build immutable report evidence.
    pub fn finish(mut self, complete: bool) -> Result<BroadcastQcReport, BroadcastQcError> {
        let rules = self.profile.rules.clone();
        for (index, rule) in rules.iter().enumerate() {
            match rule {
                BroadcastQcRule::Black { minimum_frames, .. } => {
                    self.flush_run(index, RunKind::Black, *minimum_frames, rule)?;
                }
                BroadcastQcRule::Freeze { minimum_frames, .. } => {
                    self.flush_run(index, RunKind::Freeze, *minimum_frames, rule)?;
                }
                BroadcastQcRule::SignalExcursion { .. } => {
                    self.flush_run(index, RunKind::Excursion, 1, rule)?;
                }
                BroadcastQcRule::LumaFlashCandidate { .. } => {}
            }
        }
        let complete =
            complete && !self.invalid_input && self.overflow_count == 0 && self.analyzed_frames > 0;
        let mut obligations = Vec::new();
        if self.profile.require_regulatory_flash_analysis {
            obligations.push(BroadcastQcObligation {
                kind: BroadcastQcObligationKind::RegulatoryPhotosensitiveFlash,
                status: BroadcastQcRuleStatus::NotTested,
                detail: "Run a broadcaster-approved current ITU-R BT.1702/PSE analyzer; luma transition candidates are not certification evidence".to_owned(),
            });
        }
        if self.profile.require_encoded_artifact_revalidation {
            obligations.push(BroadcastQcObligation {
                kind: BroadcastQcObligationKind::EncodedArtifactRevalidation,
                status: BroadcastQcRuleStatus::NotTested,
                detail: "Decode and re-scan the final encoded artifact; the in-process delivery-picture observation does not certify encoder or muxer output".to_owned(),
            });
        }
        let verdict = if !complete {
            BroadcastQcVerdict::Incomplete
        } else if self
            .findings
            .iter()
            .any(|finding| finding.severity == BroadcastQcSeverity::Fail)
        {
            BroadcastQcVerdict::Fail
        } else if !obligations.is_empty()
            || self.findings.iter().any(|finding| {
                matches!(
                    finding.severity,
                    BroadcastQcSeverity::Warn | BroadcastQcSeverity::Info
                )
            })
        {
            BroadcastQcVerdict::Warn
        } else {
            BroadcastQcVerdict::Pass
        };
        let mut report = BroadcastQcReport {
            schema_version: QC_REPORT_SCHEMA_VERSION,
            profile_id: self.profile.id,
            profile_edition: self.profile.edition,
            profile_fingerprint: self.profile_fingerprint,
            verdict,
            complete,
            analyzed_frames: self.analyzed_frames,
            first_frame: self.first_frame,
            last_frame: self.last_frame,
            finding_count: self.finding_count,
            findings: self.findings,
            overflow_count: self.overflow_count,
            obligations,
            evidence_sha256: [0; 32],
        };
        report.evidence_sha256 = report_digest(&report);
        Ok(report)
    }

    fn update_run(
        &mut self,
        index: usize,
        kind: RunKind,
        active: bool,
        frame_index: u64,
        coverage_ppm: u32,
        minimum_frames: u32,
        rule: &BroadcastQcRule,
    ) -> Result<(), BroadcastQcError> {
        let run = self.run_mut(index, kind);
        if active {
            if run.start.is_none() {
                run.start = Some(if kind == RunKind::Freeze {
                    frame_index.saturating_sub(1)
                } else {
                    frame_index
                });
            }
            run.end = frame_index;
            run.peak_coverage_ppm = run.peak_coverage_ppm.max(coverage_ppm);
            return Ok(());
        }
        self.flush_run(index, kind, minimum_frames, rule)
    }

    fn flush_run(
        &mut self,
        index: usize,
        kind: RunKind,
        minimum_frames: u32,
        rule: &BroadcastQcRule,
    ) -> Result<(), BroadcastQcError> {
        let run = std::mem::take(self.run_mut(index, kind));
        let Some(start) = run.start else {
            return Ok(());
        };
        let duration = run
            .end
            .checked_sub(start)
            .and_then(|value| value.checked_add(1))
            .ok_or(BroadcastQcError::FrameIndexOverflow)?;
        if duration >= u64::from(minimum_frames) {
            self.retain_finding(BroadcastQcFinding {
                rule_id: rule.rule_id().to_owned(),
                kind: match kind {
                    RunKind::Black => BroadcastQcFindingKind::BlackSegment,
                    RunKind::Freeze => BroadcastQcFindingKind::FreezeSegment,
                    RunKind::Excursion => BroadcastQcFindingKind::SignalExcursion,
                },
                severity: rule.severity(),
                start_frame: start,
                end_frame: run.end,
                measured_coverage_ppm: Some(run.peak_coverage_ppm),
                measured_luma_delta_millionths: None,
            });
        }
        Ok(())
    }

    fn run_mut(&mut self, index: usize, kind: RunKind) -> &mut RunState {
        match kind {
            RunKind::Black => &mut self.black_runs[index],
            RunKind::Freeze => &mut self.freeze_runs[index],
            RunKind::Excursion => &mut self.excursion_runs[index],
        }
    }

    fn retain_finding(&mut self, finding: BroadcastQcFinding) {
        self.finding_count = self.finding_count.saturating_add(1);
        if self.findings.len() < self.profile.maximum_retained_findings as usize {
            self.findings.push(finding);
        } else {
            self.overflow_count = self.overflow_count.saturating_add(1);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Black,
    Freeze,
    Excursion,
}

struct FrameMetrics {
    mean_luma: f32,
    rule_coverage_ppm: Vec<u32>,
}

fn measure_frame(
    rgba: &[[f32; 4]],
    previous: Option<&[[f32; 4]]>,
    active: QcActivePicture,
    contract: SignalComplianceContract,
    rules: &[BroadcastQcRule],
) -> Result<FrameMetrics, BroadcastQcError> {
    let active_pixels = active.active_pixels()?;
    let mut mean_luma = 0.0f64;
    let mut matches = vec![0u64; rules.len()];
    for row in active.y..active.y + active.height {
        let row_start = usize::try_from(row)
            .ok()
            .and_then(|row| row.checked_mul(active.raster_width as usize))
            .ok_or(BroadcastQcError::ExtentOverflow)?;
        for column in active.x..active.x + active.width {
            let index =
                row_start.checked_add(column as usize).ok_or(BroadcastQcError::ExtentOverflow)?;
            let pixel = rgba[index];
            if pixel.iter().any(|value| !value.is_finite()) {
                return Err(BroadcastQcError::NonFinitePixel { index });
            }
            let classification = contract.classify([pixel[0], pixel[1], pixel[2]])?;
            mean_luma += f64::from(classification.encoded_luma);
            for (rule_index, rule) in rules.iter().enumerate() {
                let matched = match rule {
                    BroadcastQcRule::Black { maximum_encoded_luma, .. } => {
                        classification.encoded_luma <= *maximum_encoded_luma
                    }
                    BroadcastQcRule::Freeze { maximum_channel_delta, .. } => previous
                        .map(|previous| {
                            pixel[..3]
                                .iter()
                                .zip(previous[index][..3].iter())
                                .any(|(left, right)| (left - right).abs() > *maximum_channel_delta)
                        })
                        .unwrap_or(false),
                    BroadcastQcRule::LumaFlashCandidate { .. } => false,
                    BroadcastQcRule::SignalExcursion { tolerance_per_mille, .. } => {
                        let tolerance = f32::from(*tolerance_per_mille) / 1_000.0;
                        pixel[..3]
                            .iter()
                            .any(|value| *value < -tolerance || *value > 1.0 + tolerance)
                    }
                };
                if matched {
                    matches[rule_index] = matches[rule_index].saturating_add(1);
                }
            }
        }
    }
    let rule_coverage_ppm = matches
        .into_iter()
        .map(|count| ratio_ppm(count, active_pixels))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(FrameMetrics {
        mean_luma: (mean_luma / active_pixels as f64) as f32,
        rule_coverage_ppm,
    })
}

fn validate_unit_float(value: f32) -> Result<(), BroadcastQcError> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        Err(BroadcastQcError::InvalidThreshold)
    } else {
        Ok(())
    }
}

fn validate_ppm(value: u32) -> Result<(), BroadcastQcError> {
    if u64::from(value) > PARTS_PER_MILLION {
        Err(BroadcastQcError::InvalidCoverage)
    } else {
        Ok(())
    }
}

fn validate_duration(value: u32) -> Result<(), BroadcastQcError> {
    if value == 0 {
        Err(BroadcastQcError::ZeroDuration)
    } else {
        Ok(())
    }
}

fn ratio_ppm(count: u64, total: u64) -> Result<u32, BroadcastQcError> {
    let scaled = u128::from(count)
        .checked_mul(u128::from(PARTS_PER_MILLION))
        .ok_or(BroadcastQcError::ExtentOverflow)?;
    u32::try_from(scaled / u128::from(total)).map_err(|_| BroadcastQcError::ExtentOverflow)
}

fn unit_to_millionths(value: f32) -> u32 {
    (value.clamp(0.0, 1.0) * PARTS_PER_MILLION as f32).round() as u32
}

fn update_profile_hash(hasher: &mut Sha256, profile: &BroadcastQcProfile) {
    hash_string(hasher, &profile.id);
    hash_string(hasher, &profile.edition);
    hasher.update(profile.source_sha256);
    hasher.update([color_space_tag(profile.signal_color_space)]);
    hasher.update([observation_tap_tag(profile.observation_tap)]);
    for value in [
        profile.active_picture.raster_width,
        profile.active_picture.raster_height,
        profile.active_picture.x,
        profile.active_picture.y,
        profile.active_picture.width,
        profile.active_picture.height,
        profile.maximum_retained_findings,
    ] {
        hasher.update(value.to_be_bytes());
    }
    hasher.update([u8::from(profile.require_regulatory_flash_analysis)]);
    hasher.update([u8::from(profile.require_encoded_artifact_revalidation)]);
    hasher.update((profile.rules.len() as u64).to_be_bytes());
    for rule in &profile.rules {
        update_rule_hash(hasher, rule);
    }
}

fn report_digest(report: &BroadcastQcReport) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(report.schema_version.to_be_bytes());
    hash_string(&mut hasher, &report.profile_id);
    hash_string(&mut hasher, &report.profile_edition);
    hasher.update(report.profile_fingerprint);
    hasher.update([verdict_tag(report.verdict), u8::from(report.complete)]);
    hasher.update(report.analyzed_frames.to_be_bytes());
    hash_optional_u64(&mut hasher, report.first_frame);
    hash_optional_u64(&mut hasher, report.last_frame);
    hasher.update(report.finding_count.to_be_bytes());
    hasher.update(report.overflow_count.to_be_bytes());
    hasher.update((report.findings.len() as u64).to_be_bytes());
    for finding in &report.findings {
        hash_string(&mut hasher, &finding.rule_id);
        hasher.update([
            finding_kind_tag(finding.kind),
            severity_tag(finding.severity),
        ]);
        hasher.update(finding.start_frame.to_be_bytes());
        hasher.update(finding.end_frame.to_be_bytes());
        hash_optional_u32(&mut hasher, finding.measured_coverage_ppm);
        hash_optional_u32(&mut hasher, finding.measured_luma_delta_millionths);
    }
    hasher.update((report.obligations.len() as u64).to_be_bytes());
    for obligation in &report.obligations {
        hasher.update([
            obligation_kind_tag(obligation.kind),
            rule_status_tag(obligation.status),
        ]);
        hash_string(&mut hasher, &obligation.detail);
    }
    hasher.finalize().into()
}

fn update_rule_hash(hasher: &mut Sha256, rule: &BroadcastQcRule) {
    match rule {
        BroadcastQcRule::Black {
            rule_id,
            maximum_encoded_luma,
            minimum_coverage_ppm,
            minimum_frames,
            severity,
        } => {
            hasher.update([0]);
            hash_string(hasher, rule_id);
            hasher.update(maximum_encoded_luma.to_bits().to_be_bytes());
            hasher.update(minimum_coverage_ppm.to_be_bytes());
            hasher.update(minimum_frames.to_be_bytes());
            hasher.update([severity_tag(*severity)]);
        }
        BroadcastQcRule::Freeze {
            rule_id,
            maximum_channel_delta,
            maximum_changed_ppm,
            minimum_frames,
            severity,
        } => {
            hasher.update([1]);
            hash_string(hasher, rule_id);
            hasher.update(maximum_channel_delta.to_bits().to_be_bytes());
            hasher.update(maximum_changed_ppm.to_be_bytes());
            hasher.update(minimum_frames.to_be_bytes());
            hasher.update([severity_tag(*severity)]);
        }
        BroadcastQcRule::LumaFlashCandidate { rule_id, minimum_mean_luma_delta, severity } => {
            hasher.update([2]);
            hash_string(hasher, rule_id);
            hasher.update(minimum_mean_luma_delta.to_bits().to_be_bytes());
            hasher.update([severity_tag(*severity)]);
        }
        BroadcastQcRule::SignalExcursion {
            rule_id,
            tolerance_per_mille,
            maximum_coverage_ppm,
            severity,
        } => {
            hasher.update([3]);
            hash_string(hasher, rule_id);
            hasher.update(tolerance_per_mille.to_be_bytes());
            hasher.update(maximum_coverage_ppm.to_be_bytes());
            hasher.update([severity_tag(*severity)]);
        }
    }
}

fn hash_string(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

fn hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_be_bytes());
        }
        None => hasher.update([0]),
    }
}

fn hash_optional_u32(hasher: &mut Sha256, value: Option<u32>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_be_bytes());
        }
        None => hasher.update([0]),
    }
}

const fn observation_tap_tag(value: BroadcastQcObservationTap) -> u8 {
    match value {
        BroadcastQcObservationTap::ProgramSignalBeforeLegalizer => 0,
        BroadcastQcObservationTap::DeliveryPictureAfterLegalizer => 1,
    }
}

const fn severity_tag(value: BroadcastQcSeverity) -> u8 {
    match value {
        BroadcastQcSeverity::Info => 0,
        BroadcastQcSeverity::Warn => 1,
        BroadcastQcSeverity::Fail => 2,
    }
}

const fn finding_kind_tag(value: BroadcastQcFindingKind) -> u8 {
    match value {
        BroadcastQcFindingKind::BlackSegment => 0,
        BroadcastQcFindingKind::FreezeSegment => 1,
        BroadcastQcFindingKind::LumaFlashCandidate => 2,
        BroadcastQcFindingKind::SignalExcursion => 3,
    }
}

const fn obligation_kind_tag(value: BroadcastQcObligationKind) -> u8 {
    match value {
        BroadcastQcObligationKind::RegulatoryPhotosensitiveFlash => 0,
        BroadcastQcObligationKind::EncodedArtifactRevalidation => 1,
    }
}

const fn rule_status_tag(value: BroadcastQcRuleStatus) -> u8 {
    match value {
        BroadcastQcRuleStatus::Pass => 0,
        BroadcastQcRuleStatus::Fail => 1,
        BroadcastQcRuleStatus::NotTested => 2,
        BroadcastQcRuleStatus::Inconclusive => 3,
        BroadcastQcRuleStatus::Unsupported => 4,
    }
}

const fn verdict_tag(value: BroadcastQcVerdict) -> u8 {
    match value {
        BroadcastQcVerdict::Pass => 0,
        BroadcastQcVerdict::Warn => 1,
        BroadcastQcVerdict::Fail => 2,
        BroadcastQcVerdict::Incomplete => 3,
    }
}

const fn color_space_tag(value: ColorSpace) -> u8 {
    match value {
        ColorSpace::Rec709 => 0,
        ColorSpace::Rec601Pal => 1,
        ColorSpace::Rec601Ntsc => 2,
        ColorSpace::Rec2100Hlg => 3,
        ColorSpace::Rec2100Pq => 4,
        ColorSpace::Srgb => 5,
        ColorSpace::Rec2020 => 6,
        ColorSpace::DisplayP3 => 7,
        ColorSpace::LinearRec709 => 8,
        ColorSpace::LinearRec2020 => 9,
        ColorSpace::LinearP3D65 => 10,
        ColorSpace::Aces2065_1 => 11,
        ColorSpace::AcesCg => 12,
        ColorSpace::AcesCct => 13,
        ColorSpace::AppleLogBt2020 => 14,
        ColorSpace::SonySLog2SGamut => 15,
        ColorSpace::SonySLog3SGamut3 => 16,
        ColorSpace::SonySLog3SGamut3Cine => 17,
        ColorSpace::ArriLogC3WideGamut3 => 18,
        ColorSpace::ArriLogC4WideGamut4 => 19,
        ColorSpace::CanonLog2CinemaGamutD55 => 20,
        ColorSpace::CanonLog3CinemaGamutD55 => 21,
        ColorSpace::PanasonicVLogVGamut => 22,
        ColorSpace::RedLog3G10WideGamutRgb => 23,
        ColorSpace::BlackmagicFilmWideGamutGen5 => 24,
        ColorSpace::DjiDLogDGamut => 25,
        ColorSpace::DavinciIntermediateWideGamut => 26,
    }
}

/// Stable profile, stream, and frame-analysis failures.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum BroadcastQcError {
    /// Profile identity or edition was blank.
    #[error("broadcast QC profile id and edition must not be blank")]
    BlankProfileIdentity,
    /// Active-picture rectangle was empty or outside the raster.
    #[error("broadcast QC active picture is invalid")]
    InvalidActivePicture,
    /// Profile rule count was empty or exceeded its bound.
    #[error("broadcast QC profile has invalid rule count {actual}")]
    InvalidRuleCount { actual: usize },
    /// Finding retention must have a positive bound.
    #[error("broadcast QC finding capacity must be positive")]
    ZeroFindingCapacity,
    /// A rule identity was blank.
    #[error("broadcast QC rule id must not be blank")]
    BlankRuleId,
    /// Two rules shared an identity.
    #[error("broadcast QC rule id '{rule_id}' is duplicated")]
    DuplicateRuleId { rule_id: String },
    /// One normalized threshold was non-finite or outside zero through one.
    #[error("broadcast QC normalized threshold is invalid")]
    InvalidThreshold,
    /// Coverage exceeded one million parts per million.
    #[error("broadcast QC coverage threshold is invalid")]
    InvalidCoverage,
    /// Signal tolerance exceeded the supported range.
    #[error("broadcast QC signal tolerance is invalid")]
    InvalidTolerance,
    /// Segment duration must be positive.
    #[error("broadcast QC segment duration must be positive")]
    ZeroDuration,
    /// Raster or frame-coordinate arithmetic overflowed.
    #[error("broadcast QC extent arithmetic overflow")]
    ExtentOverflow,
    /// Frame coordinate could not advance.
    #[error("broadcast QC frame coordinate overflow")]
    FrameIndexOverflow,
    /// Frames were not submitted in one contiguous run.
    #[error("broadcast QC expected frame {expected}, got {actual}")]
    NonContiguousFrame { expected: u64, actual: u64 },
    /// Frame buffer did not match the frozen raster.
    #[error("broadcast QC frame has {actual} pixels; expected {expected}")]
    RasterLength { expected: usize, actual: usize },
    /// Program Output contained NaN or infinity.
    #[error("broadcast QC frame pixel {index} is non-finite")]
    NonFinitePixel { index: usize },
    /// Shared signal-compliance classification failed.
    #[error(transparent)]
    Signal(#[from] mondrian_core::SignalComplianceError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(maximum_retained_findings: u32) -> BroadcastQcProfile {
        BroadcastQcProfile {
            id: "test-broadcaster".to_owned(),
            edition: "2026-01".to_owned(),
            source_sha256: [7; 32],
            signal_color_space: ColorSpace::Rec709,
            observation_tap: BroadcastQcObservationTap::DeliveryPictureAfterLegalizer,
            active_picture: QcActivePicture::full(2, 2),
            rules: vec![
                BroadcastQcRule::Black {
                    rule_id: "black".to_owned(),
                    maximum_encoded_luma: 0.01,
                    minimum_coverage_ppm: 1_000_000,
                    minimum_frames: 2,
                    severity: BroadcastQcSeverity::Warn,
                },
                BroadcastQcRule::Freeze {
                    rule_id: "freeze".to_owned(),
                    maximum_channel_delta: 0.001,
                    maximum_changed_ppm: 0,
                    minimum_frames: 3,
                    severity: BroadcastQcSeverity::Warn,
                },
                BroadcastQcRule::LumaFlashCandidate {
                    rule_id: "flash-candidate".to_owned(),
                    minimum_mean_luma_delta: 0.5,
                    severity: BroadcastQcSeverity::Info,
                },
                BroadcastQcRule::SignalExcursion {
                    rule_id: "r103-tolerance".to_owned(),
                    tolerance_per_mille: 50,
                    maximum_coverage_ppm: 10_000,
                    severity: BroadcastQcSeverity::Fail,
                },
            ],
            maximum_retained_findings,
            require_regulatory_flash_analysis: true,
            require_encoded_artifact_revalidation: false,
        }
    }

    #[test]
    fn streaming_rules_flush_segments_at_eof() {
        let black = [[0.0, 0.0, 0.0, 1.0]; 4];
        let white = [[1.0, 1.0, 1.0, 1.0]; 4];
        let mut session = BroadcastQcSession::new(profile(32)).expect("session");
        session
            .push(BroadcastQcFrame { frame_index: 100, rgba: &black })
            .expect("frame");
        session
            .push(BroadcastQcFrame { frame_index: 101, rgba: &black })
            .expect("frame");
        session
            .push(BroadcastQcFrame { frame_index: 102, rgba: &black })
            .expect("frame");
        session
            .push(BroadcastQcFrame { frame_index: 103, rgba: &white })
            .expect("frame");
        let report = session.finish(true).expect("report");
        assert_eq!(report.analyzed_frames, 4);
        assert!(report.findings.iter().any(|finding| {
            finding.kind == BroadcastQcFindingKind::BlackSegment
                && (finding.start_frame, finding.end_frame) == (100, 102)
        }));
        assert!(report.findings.iter().any(|finding| {
            finding.kind == BroadcastQcFindingKind::FreezeSegment
                && (finding.start_frame, finding.end_frame) == (100, 102)
        }));
        assert!(report.findings.iter().any(|finding| {
            finding.kind == BroadcastQcFindingKind::LumaFlashCandidate
                && (finding.start_frame, finding.end_frame) == (102, 103)
        }));
        assert_eq!(report.verdict, BroadcastQcVerdict::Warn);
    }

    #[test]
    fn encoded_artifact_revalidation_is_an_explicit_external_obligation() {
        let gray = [[0.5, 0.5, 0.5, 1.0]; 4];
        let mut selected = profile(32);
        selected.rules.truncate(1);
        selected.require_regulatory_flash_analysis = false;
        selected.require_encoded_artifact_revalidation = true;
        let mut session = BroadcastQcSession::new(selected).expect("session");
        session.push(BroadcastQcFrame { frame_index: 0, rgba: &gray }).expect("frame");
        let report = session.finish(true).expect("report");
        assert_eq!(report.verdict, BroadcastQcVerdict::Warn);
        assert_eq!(report.obligations.len(), 1);
        assert_eq!(
            report.obligations[0].kind,
            BroadcastQcObligationKind::EncodedArtifactRevalidation
        );
        assert_eq!(
            report.obligations[0].status,
            BroadcastQcRuleStatus::NotTested
        );
    }

    #[test]
    fn active_picture_excludes_letterbox() {
        let mut selected = profile(32);
        selected.active_picture = QcActivePicture {
            raster_width: 2,
            raster_height: 2,
            x: 0,
            y: 1,
            width: 2,
            height: 1,
        };
        selected.rules.truncate(1);
        selected.require_regulatory_flash_analysis = false;
        let mixed = [
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.5, 0.5, 0.5, 1.0],
            [0.5, 0.5, 0.5, 1.0],
        ];
        let mut session = BroadcastQcSession::new(selected).expect("session");
        session.push(BroadcastQcFrame { frame_index: 0, rgba: &mixed }).expect("frame");
        session.push(BroadcastQcFrame { frame_index: 1, rgba: &mixed }).expect("frame");
        let report = session.finish(true).expect("report");
        assert!(report.findings.is_empty());
        assert_eq!(report.verdict, BroadcastQcVerdict::Pass);
    }

    #[test]
    fn signal_excursion_uses_profile_tolerance_before_legalizer() {
        let legal = [[1.049, 0.0, 0.0, 1.0]; 4];
        let illegal = [[1.051, 0.0, 0.0, 1.0]; 4];
        let mut selected = profile(32);
        selected
            .rules
            .retain(|rule| matches!(rule, BroadcastQcRule::SignalExcursion { .. }));
        selected.require_regulatory_flash_analysis = false;
        let mut session = BroadcastQcSession::new(selected.clone()).expect("session");
        session.push(BroadcastQcFrame { frame_index: 0, rgba: &legal }).expect("frame");
        assert_eq!(
            session.finish(true).expect("report").verdict,
            BroadcastQcVerdict::Pass
        );

        let mut session = BroadcastQcSession::new(selected).expect("session");
        session
            .push(BroadcastQcFrame { frame_index: 0, rgba: &illegal })
            .expect("frame");
        let report = session.finish(true).expect("report");
        assert_eq!(report.verdict, BroadcastQcVerdict::Fail);
    }

    #[test]
    fn gaps_and_finding_overflow_are_incomplete() {
        let black = [[0.0, 0.0, 0.0, 1.0]; 4];
        let white = [[1.0, 1.0, 1.0, 1.0]; 4];
        let mut selected = profile(1);
        selected
            .rules
            .retain(|rule| matches!(rule, BroadcastQcRule::LumaFlashCandidate { .. }));
        selected.require_regulatory_flash_analysis = false;
        let mut session = BroadcastQcSession::new(selected).expect("session");
        session.push(BroadcastQcFrame { frame_index: 0, rgba: &black }).expect("frame");
        session.push(BroadcastQcFrame { frame_index: 1, rgba: &white }).expect("frame");
        session.push(BroadcastQcFrame { frame_index: 2, rgba: &black }).expect("frame");
        let report = session.finish(true).expect("report");
        assert_eq!(report.finding_count, 2);
        assert_eq!(report.overflow_count, 1);
        assert_eq!(report.verdict, BroadcastQcVerdict::Incomplete);

        let mut session = BroadcastQcSession::new(profile(4)).expect("session");
        session.push(BroadcastQcFrame { frame_index: 5, rgba: &black }).expect("frame");
        assert!(matches!(
            session.push(BroadcastQcFrame { frame_index: 7, rgba: &black }),
            Err(BroadcastQcError::NonContiguousFrame { .. })
        ));
        assert_eq!(
            session.finish(true).expect("report").verdict,
            BroadcastQcVerdict::Incomplete
        );
    }

    #[test]
    fn report_round_trip_and_digest_are_deterministic() {
        let frame = [[0.5, 0.5, 0.5, 1.0]; 4];
        let build = || {
            let mut selected = profile(4);
            selected.rules.truncate(1);
            selected.require_regulatory_flash_analysis = false;
            let mut session = BroadcastQcSession::new(selected).expect("session");
            session.push(BroadcastQcFrame { frame_index: 0, rgba: &frame }).expect("frame");
            session.finish(true).expect("report")
        };
        let first = build();
        let second = build();
        assert_eq!(first, second);
        let json = serde_json::to_vec(&first).expect("serialize");
        let decoded: BroadcastQcReport = serde_json::from_slice(&json).expect("deserialize");
        assert_eq!(decoded, first);
        assert!(decoded.verify_evidence());
        let mut corrupted = decoded;
        corrupted.finding_count += 1;
        assert!(!corrupted.verify_evidence());
    }
}
