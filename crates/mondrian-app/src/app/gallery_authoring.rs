//! Project Gallery authoring, Viewer comparison state, and Shot Match bridge.

use std::sync::Arc;

use image::ImageEncoder;
use mondrian_core::{
    automation::PropertyValue,
    effect_data::{EffectNode, EffectType},
    AuthoringList, GalleryGradeVersionBinding, GalleryStill, GalleryStillId, GradeGraphNode,
    GradeGraphNodeId, GradeGraphNodeKind, GradeVersion, GradeVersionOrigin, Result,
    ShotMatchEvidence, SHOT_MATCH_ALGORITHM_VERSION,
};

use super::preview_cpu_execution::PreviewCompositeOutput;
use super::product_action::{
    GalleryApplyShotMatchPayload, GalleryCaptureStillPayload, GalleryComparisonLayout,
    GalleryProductAction, GalleryRenameStillPayload, GallerySetComparisonPayload,
    GalleryStillTargetPayload,
};
use super::AppState;

/// Decoded Viewer-only reference retained for zero-repeat-decode painting.
#[derive(Debug, Clone)]
pub(crate) struct GalleryComparisonState {
    pub(crate) still_id: GalleryStillId,
    pub(crate) layout: GalleryComparisonLayout,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Arc<[u8]>,
}

impl AppState {
    pub(super) fn dispatch_gallery_product_action(
        &mut self,
        action: GalleryProductAction,
    ) -> Result<()> {
        match action {
            GalleryProductAction::CaptureStill(payload) => self.capture_gallery_still(*payload),
            GalleryProductAction::RenameStill(payload) => self.rename_gallery_still(payload),
            GalleryProductAction::RemoveStill(payload) => self.remove_gallery_still(payload),
            GalleryProductAction::SetComparison(payload) => self.set_gallery_comparison(payload),
            GalleryProductAction::ApplyShotMatch(payload) => self.apply_shot_match(*payload),
        }
    }

    /// Current decoded comparison reference for the Viewer projection.
    pub(crate) fn gallery_comparison(&self) -> Option<&GalleryComparisonState> {
        self.gallery_comparison.as_ref()
    }

    fn capture_gallery_still(&mut self, payload: GalleryCaptureStillPayload) -> Result<()> {
        let name = payload.name.trim().to_owned();
        if name.is_empty() {
            return Err(gallery_error("Gallery still name cannot be empty"));
        }
        payload.raster.validate()?;
        payload.statistics.validate()?;
        validate_and_decode_raster(&payload.raster)?;
        let sequence = self
            .active_sequence()
            .ok_or_else(|| gallery_error("no active Sequence for Gallery capture"))?;
        let source_sequence_id = sequence.id;
        let source_time = self.current_timeline_time()?.unwrap_or(sequence.playhead);
        let active_grade_versions = sequence
            .grade_definitions
            .iter()
            .map(|definition| GalleryGradeVersionBinding {
                definition_id: definition.id,
                version_id: definition.active_version,
            })
            .collect::<AuthoringList<_>>();
        let still = GalleryStill {
            id: GalleryStillId::new(),
            name,
            source_sequence_id,
            source_time,
            presentation_fingerprint: payload.presentation_fingerprint,
            active_grade_versions,
            raster: payload.raster,
            statistics: payload.statistics,
        };
        still.validate()?;
        let before = self
            .authoring
            .as_ref()
            .ok_or_else(|| gallery_error("no open Project"))?
            .document()
            .clone();
        if before.gallery.stills.len() >= mondrian_core::MAX_GALLERY_STILLS {
            return Err(gallery_error("Project Gallery reached the still limit"));
        }
        let mut after = before.clone();
        after.gallery.stills.push(still);
        self.commit_project_snapshot_command("捕获 Gallery Still", before, after)
    }

    fn rename_gallery_still(&mut self, payload: GalleryRenameStillPayload) -> Result<()> {
        let name = payload.name.trim().to_owned();
        if name.is_empty() {
            return Err(gallery_error("Gallery still name cannot be empty"));
        }
        let before = self
            .authoring
            .as_ref()
            .ok_or_else(|| gallery_error("no open Project"))?
            .document()
            .clone();
        let mut after = before.clone();
        let still = after
            .gallery
            .stills
            .iter_mut()
            .find(|still| still.id == payload.still_id)
            .ok_or_else(|| gallery_error("Gallery still does not exist"))?;
        if still.name == name {
            return Err(gallery_error("Gallery still name is unchanged"));
        }
        still.name = name;
        self.commit_project_snapshot_command("重命名 Gallery Still", before, after)
    }

    fn remove_gallery_still(&mut self, payload: GalleryStillTargetPayload) -> Result<()> {
        let before = self
            .authoring
            .as_ref()
            .ok_or_else(|| gallery_error("no open Project"))?
            .document()
            .clone();
        let mut after = before.clone();
        let before_len = after.gallery.stills.len();
        after.gallery.stills.retain(|still| still.id != payload.still_id);
        if after.gallery.stills.len() == before_len {
            return Err(gallery_error("Gallery still does not exist"));
        }
        self.commit_project_snapshot_command("删除 Gallery Still", before, after)?;
        if self
            .gallery_comparison
            .as_ref()
            .is_some_and(|state| state.still_id == payload.still_id)
        {
            self.gallery_comparison = None;
        }
        Ok(())
    }

    fn set_gallery_comparison(&mut self, payload: GallerySetComparisonPayload) -> Result<()> {
        if !payload.layout.validate() {
            return Err(gallery_error("Gallery comparison layout is invalid"));
        }
        let Some(still_id) = payload.still_id else {
            self.gallery_comparison = None;
            return Ok(());
        };
        let still = self
            .authoring
            .as_ref()
            .and_then(|session| {
                session.document().gallery.stills.iter().find(|still| still.id == still_id)
            })
            .ok_or_else(|| gallery_error("Gallery still does not exist"))?;
        let rgba = validate_and_decode_raster(&still.raster)?;
        self.gallery_comparison = Some(GalleryComparisonState {
            still_id,
            layout: payload.layout,
            width: still.raster.width,
            height: still.raster.height,
            rgba,
        });
        Ok(())
    }

    fn apply_shot_match(&mut self, payload: GalleryApplyShotMatchPayload) -> Result<()> {
        let version_name = payload.version_name.trim().to_owned();
        if version_name.is_empty() {
            return Err(gallery_error("Shot Match version name cannot be empty"));
        }
        payload.target_statistics.validate()?;
        let reference_statistics = self
            .authoring
            .as_ref()
            .and_then(|session| {
                session
                    .document()
                    .gallery
                    .stills
                    .iter()
                    .find(|still| still.id == payload.still_id)
                    .map(|still| still.statistics.clone())
            })
            .ok_or_else(|| gallery_error("Shot Match reference still does not exist"))?;
        let solution =
            mondrian_renderer::solve_shot_match(&reference_statistics, &payload.target_statistics);
        let mut effect = mondrian_effects::instantiate_effect_node(EffectType::ColorWheel)
            .map_err(|error| gallery_error(error.to_string()))?;
        effect.instantiate_for_clip("Grade · Shot Match".to_owned());
        set_vec3_parameter(&mut effect, "gain", solution.gain_rgb)?;
        set_vec3_parameter(&mut effect, "offset", solution.offset_rgb)?;
        let evidence = ShotMatchEvidence {
            algorithm_version: SHOT_MATCH_ALGORITHM_VERSION,
            reference_still_id: payload.still_id,
            reference_statistics,
            target_statistics: payload.target_statistics,
            gain_rgb: solution.gain_rgb,
            offset_rgb: solution.offset_rgb,
        };
        evidence.validate()?;
        self.commit_active_sequence_edit("Shot Match 创建调色版本", move |sequence| {
            let definition = sequence
                .grade_definitions
                .iter_mut()
                .find(|definition| definition.id == payload.definition_id)
                .ok_or_else(|| gallery_error("Grade Definition does not exist"))?;
            if definition.versions.len() >= mondrian_core::MAX_GRADE_VERSIONS {
                return Err(gallery_error("Grade Definition reached the version limit"));
            }
            let mut graph = definition
                .active()
                .ok_or_else(|| gallery_error("active Grade Version does not exist"))?
                .graph
                .duplicate_with_fresh_author_identities()?;
            if graph.nodes.len() >= mondrian_core::MAX_GRADE_GRAPH_NODES {
                return Err(gallery_error("Grade Graph reached the node limit"));
            }
            let node_id = GradeGraphNodeId::new();
            let input = graph.output;
            graph.nodes.push(GradeGraphNode {
                id: node_id,
                kind: GradeGraphNodeKind::Effect { input, effect },
            });
            graph.output = node_id;
            let mut version = GradeVersion::new(version_name, graph);
            version.origin = GradeVersionOrigin::ShotMatch { evidence };
            let version_id = version.id;
            definition.versions.push(version);
            if payload.activate {
                definition.active_version = version_id;
            }
            Ok(sequence.id)
        })?;
        Ok(())
    }
}

/// Build the exact capture payload from one completed CPU Viewer output.
pub(crate) fn gallery_capture_payload_from_preview(
    name: impl Into<String>,
    presentation_fingerprint: [u8; 32],
    output: &PreviewCompositeOutput,
) -> Result<GalleryCaptureStillPayload> {
    let descriptor = output.working_frame.descriptor();
    let statistics = mondrian_renderer::analyze_shot_match_frame(&output.working_frame)
        .map_err(|error| gallery_error(error.to_string()))?;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            &output.rgba,
            descriptor.width,
            descriptor.height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|error| gallery_error(format!("failed to encode Gallery PNG: {error}")))?;
    Ok(GalleryCaptureStillPayload {
        name: name.into(),
        presentation_fingerprint,
        raster: mondrian_core::GalleryStillRaster {
            width: descriptor.width,
            height: descriptor.height,
            color_space: mondrian_core::GalleryRasterColorSpace::Srgb,
            png,
        },
        statistics,
    })
}

fn validate_and_decode_raster(raster: &mondrian_core::GalleryStillRaster) -> Result<Arc<[u8]>> {
    raster.validate()?;
    let decoded = image::load_from_memory_with_format(&raster.png, image::ImageFormat::Png)
        .map_err(|error| gallery_error(format!("invalid Gallery PNG: {error}")))?
        .into_rgba8();
    if decoded.width() != raster.width || decoded.height() != raster.height {
        return Err(gallery_error(
            "Gallery PNG dimensions do not match authored metadata",
        ));
    }
    Ok(Arc::from(decoded.into_raw()))
}

fn set_vec3_parameter(effect: &mut EffectNode, name: &str, value: [f32; 3]) -> Result<()> {
    let parameter = EffectType::ColorWheel
        .parameter_id(name)
        .map_err(|error| gallery_error(error.to_string()))?;
    effect.set_static_value_by_parameter(
        &parameter,
        PropertyValue::Vec3(glam::Vec3::from_array(value)),
    )
}

fn gallery_error(reason: impl Into<String>) -> mondrian_core::MondrianError {
    mondrian_core::MondrianError::WorkflowStepFailed {
        step_id: "gallery_authoring".to_owned(),
        reason: reason.into(),
    }
}
