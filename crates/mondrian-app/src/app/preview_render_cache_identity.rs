//! Canonical Preview Adapter identities for persistent working-frame reuse.
//!
//! This Module exhaustively projects resolved Preview elements and media
//! sources into versioned bytes. Execution providers, scheduling state,
//! Program Output, monitor transforms, and Viewer presentation are excluded.

use mondrian_core::{ColorEngine, Resolution, WorkingColorSpace};
use mondrian_effects::{CompiledEffectGraph, EffectCachePolicy};
use mondrian_media::{PreviewDecodeRepresentation, PreviewSourceColorContract};
use mondrian_renderer::{
    ResolvedVisualNodeMaterializationIdentity, SourceFramePreparationIntent,
    TimelineSolidColorLayer,
};
use serde::Serialize;

use super::preview_access_mode::MediaPreviewKey;
use super::preview_viewer_plan::{ResolvedPreviewElement, ResolvedPreviewTransitionInput};

/// Canonical encoding failure. Supported semantic projections contain no maps
/// or non-finite JSON numbers, so any error fails cache admission closed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PreviewRenderCacheIdentityError {
    #[error("canonical Preview render-cache identity encoding failed: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("resolved media frame has no canonical persistent source identity")]
    MissingMediaSource,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "quality", rename_all = "snake_case")]
enum CanonicalRepresentation {
    Full,
    Reduced { divisor: u32 },
    Proxy { extent: Resolution },
}

#[derive(Serialize)]
#[serde(tag = "route", rename_all = "snake_case")]
enum CanonicalPreparation<'a> {
    ColorManaged {
        working_color_space: WorkingColorSpace,
        tone_map: bool,
        engine: &'a ColorEngine,
    },
    DataTexture {
        working_color_space: WorkingColorSpace,
    },
}

#[derive(Serialize)]
struct CanonicalMediaSource<'a> {
    schema: u16,
    path: String,
    fingerprint: mondrian_media::MediaFileFingerprint,
    video_stream_index: u32,
    physical_source_extent: Resolution,
    source_sample: mondrian_core::SourceSampleTarget,
    representation: CanonicalRepresentation,
    source_color: PreviewSourceColorContract,
    camera_raw: Option<mondrian_media::CameraRawDecodeIntent>,
    logical_source_extent: Resolution,
    sample_aspect_ratio: mondrian_core::SampleAspectRatio,
    orientation: mondrian_core::PictureOrientation,
    alpha_interpretation: mondrian_core::timeline_data::AlphaInterpretation,
    preparation: CanonicalPreparation<'a>,
}

/// Provider-independent source identity used only by persistent Render Cache.
pub(crate) fn canonical_media_source_fingerprint(
    key: &MediaPreviewKey,
) -> Result<[u8; 32], PreviewRenderCacheIdentityError> {
    let source = key.decode.source();
    let representation = canonical_representation(key.decode.representation());
    let preparation = match &key.preparation_intent {
        SourceFramePreparationIntent::ColorManaged(transform) => {
            CanonicalPreparation::ColorManaged {
                working_color_space: transform.working_color_space,
                tone_map: transform.tone_map,
                engine: &transform.engine,
            }
        }
        SourceFramePreparationIntent::DataTexture { working_color_space } => {
            CanonicalPreparation::DataTexture { working_color_space: *working_color_space }
        }
    };
    let projection = CanonicalMediaSource {
        schema: 1,
        path: source.path().to_string_lossy().into_owned(),
        fingerprint: source.fingerprint(),
        video_stream_index: source.video_stream_index(),
        physical_source_extent: source.source_extent(),
        source_sample: key.decode.source_sample(),
        representation,
        source_color: key.decode.source_color(),
        camera_raw: key.decode.camera_raw(),
        logical_source_extent: key.source_resolution,
        sample_aspect_ratio: key.picture_geometry.sample_aspect_ratio(),
        orientation: key.picture_geometry.orientation(),
        alpha_interpretation: key.alpha_interpretation,
        preparation,
    };
    let bytes = serde_json::to_vec(&projection)?;
    Ok(sha256_domain(
        b"mondrian.preview.canonical-media-source.v1",
        &bytes,
    ))
}

fn canonical_representation(
    representation: PreviewDecodeRepresentation,
) -> CanonicalRepresentation {
    match representation {
        PreviewDecodeRepresentation::NativeCpu
        | PreviewDecodeRepresentation::CompactCpuYuv
        | PreviewDecodeRepresentation::NativeSurface => CanonicalRepresentation::Full,
        PreviewDecodeRepresentation::Reduced { divisor }
        | PreviewDecodeRepresentation::ReducedCompactCpuYuv { divisor } => {
            CanonicalRepresentation::Reduced { divisor: divisor.get() }
        }
        PreviewDecodeRepresentation::Proxy(extent) => CanonicalRepresentation::Proxy { extent },
    }
}

/// Exhaustive materialization identity for one resolved Preview node.
pub(crate) fn canonical_resolved_node_materialization(
    elements: &[ResolvedPreviewElement],
) -> Result<ResolvedVisualNodeMaterializationIdentity, PreviewRenderCacheIdentityError> {
    let mut bytes = CanonicalBytes::new(b"mondrian.preview.resolved-node.v1");
    bytes.u64(elements.len() as u64);
    for element in elements {
        match element {
            ResolvedPreviewElement::SolidColor(layer) => {
                bytes.u8(0);
                encode_solid(layer, &mut bytes)?;
            }
            ResolvedPreviewElement::HeterogeneousSolidColor { layer, prepared_route: _ } => {
                bytes.u8(1);
                encode_solid(layer, &mut bytes)?;
            }
            ResolvedPreviewElement::Adjustment(adjustment) => {
                bytes.u8(2);
                bytes.f32(adjustment.opacity);
                bytes.json(&adjustment.blend_mode)?;
                encode_effect(&adjustment.effect_graph, adjustment.frame_seed, &mut bytes)?;
            }
            ResolvedPreviewElement::Media {
                frame,
                opacity,
                blend_mode,
                transform,
                effect_graph,
                prepared_heterogeneous_route: _,
                frame_seed,
            } => {
                bytes.u8(3);
                bytes.fixed(
                    &frame
                        .render_cache_source_fingerprint()
                        .ok_or(PreviewRenderCacheIdentityError::MissingMediaSource)?,
                );
                bytes.u32(frame.width());
                bytes.u32(frame.height());
                bytes.u32(frame.logical_resolution().width);
                bytes.u32(frame.logical_resolution().height);
                bytes.f32(*opacity);
                bytes.json(blend_mode)?;
                encode_transform(*transform, &mut bytes);
                encode_effect(effect_graph, *frame_seed, &mut bytes)?;
            }
            ResolvedPreviewElement::CrossDissolve { left, right, progress } => {
                bytes.u8(4);
                encode_transition(left, &mut bytes)?;
                encode_transition(right, &mut bytes)?;
                bytes.f32(*progress);
            }
        }
    }
    Ok(ResolvedVisualNodeMaterializationIdentity::from_canonical_bytes(&bytes.finish()))
}

fn encode_transition(
    input: &ResolvedPreviewTransitionInput,
    bytes: &mut CanonicalBytes,
) -> Result<(), PreviewRenderCacheIdentityError> {
    match input {
        ResolvedPreviewTransitionInput::Transparent => bytes.u8(0),
        ResolvedPreviewTransitionInput::SolidColor(layer) => {
            bytes.u8(1);
            encode_solid(layer, bytes)?;
        }
        ResolvedPreviewTransitionInput::HeterogeneousSolidColor { layer, prepared_route: _ } => {
            bytes.u8(2);
            encode_solid(layer, bytes)?;
        }
        ResolvedPreviewTransitionInput::Media {
            frame,
            opacity,
            blend_mode,
            transform,
            effect_graph,
            prepared_heterogeneous_route: _,
            frame_seed,
        } => {
            bytes.u8(3);
            bytes.fixed(
                &frame
                    .render_cache_source_fingerprint()
                    .ok_or(PreviewRenderCacheIdentityError::MissingMediaSource)?,
            );
            bytes.u32(frame.width());
            bytes.u32(frame.height());
            bytes.u32(frame.logical_resolution().width);
            bytes.u32(frame.logical_resolution().height);
            bytes.f32(*opacity);
            bytes.json(blend_mode)?;
            encode_transform(*transform, bytes);
            encode_effect(effect_graph, *frame_seed, bytes)?;
        }
    }
    Ok(())
}

fn encode_solid(
    layer: &TimelineSolidColorLayer,
    bytes: &mut CanonicalBytes,
) -> Result<(), PreviewRenderCacheIdentityError> {
    bytes.f32(layer.color.r);
    bytes.f32(layer.color.g);
    bytes.f32(layer.color.b);
    bytes.f32(layer.color.a);
    bytes.f32(layer.opacity);
    bytes.json(&layer.blend_mode)?;
    encode_transform(layer.transform, bytes);
    encode_effect(&layer.effect_graph, layer.frame_seed, bytes)
}

fn encode_transform(transform: [f32; 6], bytes: &mut CanonicalBytes) {
    for value in transform {
        bytes.f32(value);
    }
}

fn encode_effect(
    graph: &CompiledEffectGraph,
    frame_seed: i64,
    bytes: &mut CanonicalBytes,
) -> Result<(), PreviewRenderCacheIdentityError> {
    bytes.fixed(&graph.semantic_fingerprint());
    let policy = graph.output_cache_policy();
    bytes.json(&policy)?;
    if policy == EffectCachePolicy::FrameDependent {
        bytes.i64(frame_seed);
    }
    Ok(())
}

struct CanonicalBytes(Vec<u8>);

impl CanonicalBytes {
    fn new(domain: &[u8]) -> Self {
        let mut bytes = Self(Vec::new());
        bytes.field(domain);
        bytes
    }

    fn finish(self) -> Vec<u8> {
        self.0
    }

    fn field(&mut self, value: &[u8]) {
        self.0.extend_from_slice(&(value.len() as u64).to_le_bytes());
        self.0.extend_from_slice(value);
    }

    fn fixed(&mut self, value: &[u8; 32]) {
        self.0.extend_from_slice(value);
    }

    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn f32(&mut self, value: f32) {
        self.u32(value.to_bits());
    }

    fn json<T: Serialize + ?Sized>(
        &mut self,
        value: &T,
    ) -> Result<(), PreviewRenderCacheIdentityError> {
        self.field(&serde_json::to_vec(value)?);
        Ok(())
    }
}

fn sha256_domain(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update((domain.len() as u64).to_le_bytes());
    hasher.update(domain);
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{ColorEngine, ColorSpace, TimelineTime};
    use mondrian_media::{DecodedVideoRange, PreviewSourceColorContract};
    use std::num::NonZeroU32;

    fn media_key(seed: u64) -> MediaPreviewKey {
        MediaPreviewKey::test_cpu(
            std::path::PathBuf::from(format!("canonical-source-{seed}.mov")),
            MediaPreviewKey::test_fingerprint(seed),
            TimelineTime::new(7, 24).expect("time"),
            Resolution { width: 1920, height: 1080 },
            PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited),
        )
    }

    #[test]
    fn execution_backend_does_not_rotate_persistent_source_identity() {
        let cpu = media_key(7);
        let mut gpu = cpu.clone();
        gpu.preparation_intent = mondrian_renderer::RenderInputTransform::to_working_gpu(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::mondrian_standard(),
        )
        .into();

        assert_eq!(
            canonical_media_source_fingerprint(&cpu).expect("CPU identity"),
            canonical_media_source_fingerprint(&gpu).expect("GPU identity")
        );
    }

    #[test]
    fn source_revision_rotates_persistent_source_identity() {
        assert_ne!(
            canonical_media_source_fingerprint(&media_key(11)).expect("first identity"),
            canonical_media_source_fingerprint(&media_key(12)).expect("second identity")
        );
    }

    #[test]
    fn representation_projection_normalizes_provider_but_preserves_quality() {
        assert_eq!(
            canonical_representation(PreviewDecodeRepresentation::NativeCpu),
            canonical_representation(PreviewDecodeRepresentation::NativeSurface)
        );
        assert_eq!(
            canonical_representation(PreviewDecodeRepresentation::CompactCpuYuv),
            CanonicalRepresentation::Full
        );
        let divisor = NonZeroU32::new(2).expect("nonzero");
        assert_eq!(
            canonical_representation(PreviewDecodeRepresentation::Reduced { divisor }),
            canonical_representation(PreviewDecodeRepresentation::ReducedCompactCpuYuv { divisor })
        );
        assert_ne!(
            canonical_representation(PreviewDecodeRepresentation::NativeCpu),
            canonical_representation(PreviewDecodeRepresentation::Reduced { divisor })
        );
    }
}
