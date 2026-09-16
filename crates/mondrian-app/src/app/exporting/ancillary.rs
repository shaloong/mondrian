//! Bounded explicit import of the Broadcast-owned immutable ANC schema.
use super::AppState;
use mondrian_broadcast::{CaptionImportBinding, CaptionSourceFormat, FrozenAncillaryProgram};
use mondrian_core::{FramePosition, MondrianError, Rational, Result, TimelineTime};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_IMPORT_BYTES: u64 = 8 * 1024 * 1024;

/// File selection and immutable validated canonical program retained by a draft.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedAncillaryProgram {
    /// Original explicit selection, used only for display after import.
    pub path: PathBuf,
    /// Digest of the exact imported source bytes.
    pub source_sha256: [u8; 32],
    /// Broadcast authority; subsequent source-file edits cannot mutate a job.
    pub program: Arc<FrozenAncillaryProgram>,
    /// Validated inventory count cached for inexpensive UI snapshots.
    pub packet_count: usize,
}

impl ImportedAncillaryProgram {
    fn read(path: &Path) -> Result<Self> {
        Self::read_with_binding(path, None)
    }
    fn read_with_binding(path: &Path, binding: Option<CaptionImportBinding>) -> Result<Self> {
        let failure = |reason: String| MondrianError::WorkflowStepFailed {
            step_id: "import_export_ancillary".to_owned(),
            reason,
        };
        let file = std::fs::File::open(path).map_err(|error| failure(error.to_string()))?;
        let metadata = file.metadata().map_err(|error| failure(error.to_string()))?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_IMPORT_BYTES {
            return Err(failure(
                "ANC / 字幕必须是 1 字节至 8 MiB 的普通文件".to_owned(),
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_IMPORT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| failure(error.to_string()))?;
        if bytes.len() as u64 > MAX_IMPORT_BYTES {
            return Err(failure("读取期间 ANC 文件超过 8 MiB 上限".to_owned()));
        }
        let program: FrozenAncillaryProgram = if let Some(format) = caption_format(path) {
            mondrian_broadcast::import_caption_program(
                &bytes,
                format,
                binding
                    .ok_or_else(|| failure("字幕导入需要先选择支持的导出格式和范围".to_owned()))?,
            )
            .map_err(|error| failure(error.to_string()))?
        } else {
            if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("mcc")) {
                return Err(failure(
                    "尚不支持 MCC；请选择 SCC V1.0 或原始 data-only CDP".to_owned(),
                ));
            }
            serde_json::from_slice(&bytes)
                .map_err(|error| failure(format!("无效 canonical ANC JSON：{error}")))?
        };
        program
            .validate()
            .map_err(|error| failure(format!("ANC / ST436 预检失败：{error}")))?;
        let packet_count = program.packet_count();
        Ok(Self {
            path: path.to_path_buf(),
            source_sha256: Sha256::digest(&bytes).into(),
            program: Arc::new(program),
            packet_count,
        })
    }
}

fn caption_format(path: &Path) -> Option<CaptionSourceFormat> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "scc" => Some(CaptionSourceFormat::ScenaristSccV1),
        "cdp" => Some(CaptionSourceFormat::RawCdpSt334_2_2015),
        _ => None,
    }
}

impl AppState {
    /// Inexpensive current-draft readiness using Export's shared selection resolver.
    pub fn can_enqueue_export_draft(&self) -> bool {
        if self.export_draft.output_path.trim().is_empty() {
            return false;
        }
        let Some(sequence) = self
            .export_draft
            .selected_sequence_id
            .and_then(|id| self.sequence_by_id(id))
            .or_else(|| self.active_sequence())
            .or_else(|| self.default_sequence_id().and_then(|id| self.sequence_by_id(id)))
        else {
            return false;
        };
        if mondrian_export::delivery::resolve_export_delivery(
            &self.export_draft.preset,
            &sequence.settings,
            self.project_color_environment(),
        )
        .is_err()
        {
            return false;
        }
        self.export_draft.ancillary.as_ref().is_none_or(|ancillary| {
            mondrian_export::queue::check_ancillary_export_selection(
                &ancillary.program,
                &self.export_draft.preset,
                sequence,
                self.export_draft.range,
                self.project_color_environment(),
            )
            .is_ok()
        })
    }
    /// Freeze an explicit draft attachment only once on enqueue, keeping large
    /// ANC payload cloning and serialization out of UI paint/availability work.
    pub fn enqueue_current_export_draft(&mut self) -> Result<mondrian_core::JobId> {
        let sequence_id = self
            .export_draft
            .selected_sequence_id
            .filter(|id| self.sequence_by_id(*id).is_some())
            .or(self.active_sequence_id())
            .or(self.default_sequence_id());
        let request = super::TimelineExportRequest {
            preset: self.export_draft.preset.clone(),
            sequence_id,
            range: self.export_draft.range,
            output_path: self.export_draft.output_path.trim().into(),
            output_policy: mondrian_export::preset::ExportOutputPolicy::CreateNew,
            broadcast_qc: self
                .export_draft
                .regulatory_pse
                .as_ref()
                .map(|selected| selected.qc_profile.clone()),
            regulatory_pse: self
                .export_draft
                .regulatory_pse
                .as_ref()
                .map(|selected| selected.provider.clone()),
            frozen_ancillary: self
                .export_draft
                .ancillary
                .as_ref()
                .map(|item| item.program.as_ref().clone()),
        };
        self.enqueue_timeline_export(request)
    }
    /// Read and validate an explicit selection before atomically changing draft
    /// state. Failed imports leave the previous immutable selection intact.
    pub fn import_export_draft_ancillary(&mut self, path: PathBuf) -> Result<bool> {
        let imported = if caption_format(&path).is_some() {
            let failure = |reason: String| MondrianError::WorkflowStepFailed {
                step_id: "import_export_ancillary".to_owned(),
                reason,
            };
            let sequence = self
                .export_draft
                .selected_sequence_id
                .and_then(|id| self.sequence_by_id(id))
                .or_else(|| self.active_sequence())
                .or_else(|| self.default_sequence_id().and_then(|id| self.sequence_by_id(id)))
                .ok_or_else(|| failure("请先选择要导出的序列".to_owned()))?;
            let selection = mondrian_export::queue::resolve_ancillary_export_selection(
                &self.export_draft.preset,
                sequence,
                self.export_draft.range,
                self.project_color_environment(),
            )
            .map_err(failure)?;
            let timecode_origin = TimelineTime::from_frame_position(FramePosition::new(
                sequence.settings.timeline_display.timecode_start_frame,
                Rational::new(
                    sequence.settings.frame_rate.den,
                    sequence.settings.frame_rate.num,
                ),
            ))
            .map_err(|error| failure(error.to_string()))?;
            ImportedAncillaryProgram::read_with_binding(
                &path,
                Some(CaptionImportBinding {
                    source_start: selection.source_start,
                    output_frame_rate: selection.output_frame_rate,
                    frame_count: selection.frame_count,
                    timecode_origin,
                    placement: mondrian_broadcast::AncillaryPlacement::new(
                        mondrian_broadcast::AncillarySpace::Vanc,
                        mondrian_broadcast::AncillaryField::Progressive,
                        20,
                        0,
                    )
                    .map_err(|error| failure(error.to_string()))?,
                }),
            )?
        } else {
            ImportedAncillaryProgram::read(&path)?
        };
        let changed = self.export_draft.ancillary.as_ref() != Some(&imported);
        if changed {
            self.export_draft.ancillary = Some(imported);
        }
        Ok(changed)
    }
    /// Remove the selected ancillary attachment from future export submissions.
    pub fn clear_export_draft_ancillary(&mut self) -> bool {
        self.export_draft.ancillary.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn import_freezes_bytes_and_rejects_unknown_fields_trailing_and_unbounded_files() {
        let work = tempfile::tempdir().expect("work");
        let path = work.path().join("ancillary.json");
        let program = FrozenAncillaryProgram::new(
            mondrian_core::TimelineTime::ZERO,
            mondrian_core::Rational::new(25, 1),
            3,
            vec![],
        )
        .expect("program");
        let bytes = serde_json::to_vec(&program).expect("JSON");
        std::fs::write(&path, &bytes).expect("file");
        let imported = ImportedAncillaryProgram::read(&path).expect("import");
        assert_eq!(imported.program.frame_count(), 3);
        std::fs::write(&path, b"{}").expect("external mutation");
        assert_eq!(imported.program.frame_count(), 3);
        assert!(ImportedAncillaryProgram::read(&path).is_err());
        let mut value = serde_json::to_value(&program).expect("value");
        value["unexpected"] = true.into();
        std::fs::write(&path, serde_json::to_vec(&value).expect("JSON")).expect("file");
        assert!(ImportedAncillaryProgram::read(&path).is_err());
        std::fs::write(&path, [bytes.as_slice(), b"{}"].concat()).expect("trailing");
        assert!(ImportedAncillaryProgram::read(&path).is_err());
        std::fs::File::create(&path)
            .expect("file")
            .set_len(MAX_IMPORT_BYTES + 1)
            .expect("extent");
        assert!(ImportedAncillaryProgram::read(&path).is_err());
    }
}
