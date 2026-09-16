//! Product composition for timeline interchange.
//!
//! Parsing/format semantics remain in `mondrian-interchange`. This Adapter
//! resolves the Asset Library, appends an imported Sequence through exactly one
//! Project author transaction, and prepares exports from immutable snapshots.

use super::AppState;
use mondrian_core::{AssetId, MondrianError};
use mondrian_interchange::{
    materialize_import, prepare_export, InterchangeAssetSnapshot, InterchangeConformanceReport,
    InterchangeExportRequest, InterchangeFormatProfile, InterchangeImportCandidate,
    InterchangeLimits, InterchangeLossPolicy, InterchangeMediaBinding, PreparedInterchangeArtifact,
    PreparedInterchangeImport,
};
use std::collections::BTreeSet;

impl AppState {
    /// Append one already-inspected interchange candidate as a new Sequence in
    /// exactly one reversible Project author transaction.
    ///
    /// Open-Session navigation remains unchanged by Project snapshot commits;
    /// the caller may dispatch the ordinary Sequence switch action afterward.
    pub fn import_prepared_timeline_interchange(
        &mut self,
        prepared: &PreparedInterchangeImport,
        bindings: &[InterchangeMediaBinding],
        loss_policy: InterchangeLossPolicy,
    ) -> mondrian_core::Result<InterchangeConformanceReport> {
        let library = self
            .asset_library()
            .ok_or_else(|| interchange_workflow_error("no Asset Library is open"))?;
        for binding in bindings {
            if library.get_asset(binding.asset_id)?.is_none() {
                return Err(missing_asset_error(binding.asset_id));
            }
        }
        let InterchangeImportCandidate { sequence, report, .. } =
            materialize_import(prepared, bindings, loss_policy).map_err(interchange_error)?;
        let before = self
            .authoring
            .as_ref()
            .ok_or_else(|| interchange_workflow_error("no Project is open"))?
            .document()
            .clone();
        sequence.validate_author_contract(&before.color_environment)?;
        let mut after = before.clone();
        after.sequences.add_sequence(sequence)?;
        self.stop()?;
        self.commit_project_snapshot_command("导入时间线交换文件", before, after)?;
        self.settle_preview_access_source();
        Ok(report)
    }

    /// Prepare one native interchange artifact from the active canonical
    /// Sequence and immutable Asset Library records. No filesystem publication
    /// occurs in this method.
    pub fn prepare_active_timeline_interchange(
        &self,
        profile: InterchangeFormatProfile,
        loss_policy: InterchangeLossPolicy,
        limits: InterchangeLimits,
    ) -> mondrian_core::Result<PreparedInterchangeArtifact> {
        let sequence = self
            .active_sequence()
            .ok_or_else(|| interchange_workflow_error("no active Sequence"))?;
        let library = self
            .asset_library()
            .ok_or_else(|| interchange_workflow_error("no Asset Library is open"))?;
        let asset_ids = sequence
            .video_tracks
            .iter()
            .chain(&sequence.audio_tracks)
            .flat_map(|track| &track.clips)
            .filter_map(|clip| clip.media_asset_id())
            .collect::<BTreeSet<_>>();
        let mut assets = Vec::with_capacity(asset_ids.len());
        for asset_id in asset_ids {
            let record =
                library.get_asset(asset_id)?.ok_or_else(|| missing_asset_error(asset_id))?;
            let locator = record.file_path().map(|path| path.to_string_lossy().into_owned());
            assets.push(InterchangeAssetSnapshot {
                asset_id,
                name: record.name,
                locator,
                editorial_source: None,
                color_space: record.interpretation.color.override_color_space(),
            });
        }
        prepare_export(InterchangeExportRequest {
            profile,
            sequence,
            assets: &assets,
            loss_policy,
            limits,
        })
        .map_err(interchange_error)
    }
}

fn interchange_error(error: mondrian_interchange::InterchangeError) -> MondrianError {
    interchange_workflow_error(error.to_string())
}

fn interchange_workflow_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "timeline_interchange".to_owned(),
        reason: reason.into(),
    }
}

fn missing_asset_error(asset_id: AssetId) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "timeline_interchange_asset_snapshot".to_owned(),
        reason: format!("Asset {asset_id} is missing from the canonical Asset Library"),
    }
}
