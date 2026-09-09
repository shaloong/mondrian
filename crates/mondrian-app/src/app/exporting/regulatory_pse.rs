//! Explicit import of a third-party approved PSE configuration and frozen QC profile.
use super::AppState;
use mondrian_core::{MondrianError, Result};
use serde::{Deserialize, Serialize};
use std::{io::Read, path::PathBuf};

/// Commissioned external configuration selected for subsequent export jobs.
/// Import validates structure and binding, never grants or manufactures approval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedRegulatoryPseConfiguration {
    /// External installation paths and approval trust anchor.
    pub provider: mondrian_export::RegulatoryPseProviderConfig,
    /// Exact delivery QC profile approved for this installation.
    pub qc_profile: mondrian_broadcast::BroadcastQcProfile,
}

impl AppState {
    /// Freeze a bounded explicitly selected JSON configuration; failure preserves the previous draft.
    pub fn import_export_draft_regulatory_pse(&mut self, path: PathBuf) -> Result<bool> {
        let failure = |reason: String| MondrianError::WorkflowStepFailed {
            step_id: "import_export_regulatory_pse".to_owned(),
            reason,
        };
        let file = std::fs::File::open(path).map_err(|error| failure(error.to_string()))?;
        let metadata = file.metadata().map_err(|error| failure(error.to_string()))?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 1024 * 1024 {
            return Err(failure(
                "PSE configuration must be a nonempty regular JSON file no larger than 1 MiB"
                    .to_owned(),
            ));
        }
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| failure(error.to_string()))?;
        if bytes.len() > 1024 * 1024 {
            return Err(failure(
                "PSE configuration grew beyond its bound".to_owned(),
            ));
        }
        let imported: ImportedRegulatoryPseConfiguration =
            serde_json::from_slice(&bytes).map_err(|error| failure(error.to_string()))?;
        let fingerprint =
            imported.qc_profile.fingerprint().map_err(|error| failure(error.to_string()))?;
        if !imported.qc_profile.require_regulatory_flash_analysis
            || !imported.provider.approval.validate()
            || imported.provider.approval.qc_profile_fingerprint != fingerprint
        {
            return Err(failure("PSE configuration has no frozen external approval matching this regulatory QC profile".to_owned()));
        }
        let changed = self.export_draft.regulatory_pse.as_ref() != Some(&imported);
        if changed {
            self.export_draft.regulatory_pse = Some(imported);
        }
        Ok(changed)
    }

    /// Clear the externally commissioned PSE configuration for future jobs.
    pub fn clear_export_draft_regulatory_pse(&mut self) -> bool {
        self.export_draft.regulatory_pse.take().is_some()
    }
}
