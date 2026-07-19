//! Stable asset-owned bindings from authored audio components to probed streams.

use mondrian_core::AudioSourceComponentId;
use mondrian_media::{
    info::ChannelLayout, AudioSourceSelection, AudioStreamInfo, MediaFileFingerprint, MediaInfo,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiserror::Error;

/// Stable logical audio component exposed by one Asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetAudioComponent {
    /// Author-facing identity referenced by Timeline Component Edits.
    pub id: AudioSourceComponentId,
    /// Conservative binding to one physical stream in the current source.
    pub binding: AssetAudioStreamBinding,
}

/// Persisted physical-stream signature expected by one Asset Component.
///
/// The stream index is the concrete FFmpeg selection. The remaining fields are
/// guards against silently retargeting the Component after relink or reprobe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetAudioStreamBinding {
    /// Absolute container stream index used by `-map 0:<index>`.
    pub stream_index: u32,
    /// Container stream identifier when present.
    pub stream_id: Option<i32>,
    /// Declared source channel layout. Unknown remains unknown.
    pub channel_layout: ChannelLayout,
    /// Normalized language metadata used as a conservative identity guard.
    pub language: Option<String>,
}

impl AssetAudioStreamBinding {
    /// Capture a binding from one probed physical stream.
    pub fn from_stream(stream: &AudioStreamInfo) -> Self {
        Self {
            stream_index: stream.index,
            stream_id: stream.stream_id,
            channel_layout: stream.channel_layout.clone(),
            language: stream.language.clone(),
        }
    }

    /// Whether current probe evidence still proves the same physical binding.
    pub fn matches_stream(&self, stream: &AudioStreamInfo) -> bool {
        self.stream_index == stream.index
            && self.stream_id == stream.stream_id
            && self.channel_layout == stream.channel_layout
            && self.language == stream.language
    }
}

/// Asset-owned stable audio component catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetAudioComponentCatalog {
    /// File revision whose probe evidence produced these bindings.
    #[serde(default)]
    pub source_fingerprint: MediaFileFingerprint,
    /// Stable components. The primary logical Component is ordered first.
    pub components: Vec<AssetAudioComponent>,
}

impl AssetAudioComponentCatalog {
    /// Create a catalog for a newly imported Asset.
    pub fn from_media_info(info: &MediaInfo, source_fingerprint: MediaFileFingerprint) -> Self {
        let Some(primary_index) = preferred_audio_stream_index(info) else {
            return Self { source_fingerprint, components: Vec::new() };
        };
        let mut components = Vec::with_capacity(info.audio_streams.len());
        for (index, stream) in info.audio_streams.iter().enumerate() {
            let id = if index == primary_index {
                AudioSourceComponentId::primary()
            } else {
                AudioSourceComponentId::new()
            };
            components.push(AssetAudioComponent {
                id,
                binding: AssetAudioStreamBinding::from_stream(stream),
            });
        }
        sort_components(&mut components);
        Self { source_fingerprint, components }
    }

    /// Preserve every existing authored identity while discovering genuinely
    /// new physical streams. A conflicting stream at an already-bound index is
    /// not auto-added or retargeted; explicit user rebinding must resolve it.
    pub fn reconcile(
        &self,
        info: &MediaInfo,
        source_fingerprint: MediaFileFingerprint,
    ) -> Result<Self, AudioComponentCatalogError> {
        self.validate()?;
        let mut components = self.components.clone();
        for stream in &info.audio_streams {
            let exact_binding_exists =
                components.iter().any(|component| component.binding.matches_stream(stream));
            let index_is_already_claimed = components
                .iter()
                .any(|component| component.binding.stream_index == stream.index);
            if !exact_binding_exists && !index_is_already_claimed {
                components.push(AssetAudioComponent {
                    id: AudioSourceComponentId::new(),
                    binding: AssetAudioStreamBinding::from_stream(stream),
                });
            }
        }
        sort_components(&mut components);
        let catalog = Self { source_fingerprint, components };
        catalog.validate()?;
        Ok(catalog)
    }

    /// Explicitly bind one existing logical Component to a probed physical stream.
    ///
    /// Rebinding preserves `component_id`, so Timeline edits and projects that
    /// reference it do not need to be rewritten. Another logical Component may
    /// already name the same physical stream: explicit aliases are valid, while
    /// [`Self::reconcile`] never creates one implicitly.
    pub fn rebind(
        &self,
        component_id: AudioSourceComponentId,
        stream_index: u32,
        info: &MediaInfo,
        source_fingerprint: MediaFileFingerprint,
    ) -> Result<Self, AudioComponentCatalogError> {
        self.validate()?;
        let stream = info
            .audio_streams
            .iter()
            .find(|stream| stream.index == stream_index)
            .ok_or(AudioComponentCatalogError::UnknownPhysicalStream { stream_index })?;
        let mut components = self.components.clone();
        let component = components
            .iter_mut()
            .find(|component| component.id == component_id)
            .ok_or(AudioComponentCatalogError::UnknownComponent { component_id })?;
        component.binding = AssetAudioStreamBinding::from_stream(stream);
        sort_components(&mut components);
        let catalog = Self { source_fingerprint, components };
        catalog.validate()?;
        Ok(catalog)
    }

    /// Resolve a stable logical Component against current probe evidence.
    pub fn resolve<'a>(
        &self,
        component_id: AudioSourceComponentId,
        info: &'a MediaInfo,
    ) -> Result<&'a AudioStreamInfo, AudioComponentCatalogError> {
        self.validate()?;
        let component = self
            .components
            .iter()
            .find(|component| component.id == component_id)
            .ok_or(AudioComponentCatalogError::UnknownComponent { component_id })?;
        let stream = info
            .audio_streams
            .iter()
            .find(|stream| stream.index == component.binding.stream_index)
            .ok_or(AudioComponentCatalogError::MissingStream {
                component_id,
                stream_index: component.binding.stream_index,
            })?;
        if !component.binding.matches_stream(stream) {
            return Err(AudioComponentCatalogError::StreamBindingDrift {
                component_id,
                stream_index: component.binding.stream_index,
            });
        }
        Ok(stream)
    }

    /// Resolve one stable Component into a physical media execution selection.
    pub fn resolve_selection(
        &self,
        component_id: AudioSourceComponentId,
        info: &MediaInfo,
    ) -> Result<AudioSourceSelection, AudioComponentCatalogError> {
        self.resolve(component_id, info)
            .map(|stream| AudioSourceSelection::from_stream(stream, self.source_fingerprint))
    }

    /// Resolve only when current filesystem evidence matches the revision that
    /// produced the persisted stream catalog.
    pub fn resolve_current_selection(
        &self,
        component_id: AudioSourceComponentId,
        info: &MediaInfo,
        current_fingerprint: MediaFileFingerprint,
    ) -> Result<AudioSourceSelection, AudioComponentCatalogError> {
        if current_fingerprint != self.source_fingerprint {
            return Err(AudioComponentCatalogError::SourceRevisionDrift);
        }
        self.resolve_selection(component_id, info)
    }

    /// Validate stable logical Component identity uniqueness.
    ///
    /// Multiple IDs may deliberately alias one physical stream after explicit
    /// rebinding. Automatic import/reconciliation still creates at most one ID
    /// per discovered stream.
    pub fn validate(&self) -> Result<(), AudioComponentCatalogError> {
        let mut ids = BTreeSet::new();
        for component in &self.components {
            if !ids.insert(component.id) {
                return Err(AudioComponentCatalogError::DuplicateComponent {
                    component_id: component.id,
                });
            }
        }
        Ok(())
    }
}

/// Fail-closed Asset Component Catalog error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AudioComponentCatalogError {
    /// Current file evidence no longer matches the source revision that was probed.
    #[error("audio source revision changed after Component binding was probed")]
    SourceRevisionDrift,
    /// Two catalog entries reused one stable identity.
    #[error("duplicate audio component identity {component_id}")]
    DuplicateComponent {
        /// Conflicting stable identity.
        component_id: AudioSourceComponentId,
    },
    /// The Timeline references a Component absent from this Asset catalog.
    #[error("unknown audio component {component_id}")]
    UnknownComponent {
        /// Missing stable identity.
        component_id: AudioSourceComponentId,
    },
    /// The current probe has no stream at the requested physical index.
    #[error("unknown physical audio stream index {stream_index}")]
    UnknownPhysicalStream {
        /// Requested absolute container stream index.
        stream_index: u32,
    },
    /// The expected physical stream is absent from current probe evidence.
    #[error("audio component {component_id} expects missing stream index {stream_index}")]
    MissingStream {
        /// Stable logical identity.
        component_id: AudioSourceComponentId,
        /// Expected physical stream index.
        stream_index: u32,
    },
    /// A stream reused the expected index but not the persisted signature.
    #[error(
        "audio component {component_id} stream binding drifted at stream index {stream_index}"
    )]
    StreamBindingDrift {
        /// Stable logical identity.
        component_id: AudioSourceComponentId,
        /// Conflicting physical stream index.
        stream_index: u32,
    },
}

fn preferred_audio_stream_index(info: &MediaInfo) -> Option<usize> {
    info.audio_streams
        .iter()
        .position(|stream| stream.is_default)
        .or_else(|| (!info.audio_streams.is_empty()).then_some(0))
}

fn sort_components(components: &mut [AssetAudioComponent]) {
    components.sort_by_key(|component| {
        (
            component.id != AudioSourceComponentId::primary(),
            component.binding.stream_index,
            component.id,
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_media::{info::AudioCodec, AudioStreamInfo};
    use std::path::PathBuf;
    use std::time::Duration;

    fn fingerprint(revision: u64) -> MediaFileFingerprint {
        MediaFileFingerprint {
            len: Some(revision),
            modified_secs: Some(revision),
            modified_nanos: Some(0),
        }
    }

    fn stream(
        index: u32,
        stream_id: i32,
        language: &str,
        layout: ChannelLayout,
        is_default: bool,
    ) -> AudioStreamInfo {
        AudioStreamInfo {
            index,
            stream_id: Some(stream_id),
            language: Some(language.to_owned()),
            title: None,
            is_default,
            codec: AudioCodec::Aac,
            duration: Some(Duration::from_secs(60)),
            sample_rate: 48_000,
            channels: layout.channel_count(),
            channel_layout: layout,
            bit_depth: 24,
            avg_bitrate: 256_000,
        }
    }

    fn media_info(audio_streams: Vec<AudioStreamInfo>) -> MediaInfo {
        MediaInfo {
            path: PathBuf::from("fixture.mov"),
            duration: Duration::from_secs(60),
            file_size: 1024,
            container: "mov".to_owned(),
            has_video: false,
            has_audio: !audio_streams.is_empty(),
            video_streams: Vec::new(),
            audio_streams,
        }
    }

    #[test]
    fn primary_component_uses_declared_default_stream_not_first_stream() {
        let info = media_info(vec![
            stream(2, 20, "eng", ChannelLayout::Stereo, false),
            stream(5, 50, "jpn", ChannelLayout::Surround51Side, true),
        ]);

        let catalog = AssetAudioComponentCatalog::from_media_info(&info, fingerprint(1));
        let resolved = catalog
            .resolve(AudioSourceComponentId::primary(), &info)
            .expect("default stream binding");

        assert_eq!(resolved.index, 5);
        assert_eq!(resolved.language.as_deref(), Some("jpn"));
    }

    #[test]
    fn relink_preserves_missing_and_drifted_author_bindings_without_retargeting() {
        let original = media_info(vec![
            stream(1, 10, "eng", ChannelLayout::Stereo, true),
            stream(3, 30, "fra", ChannelLayout::Stereo, false),
        ]);
        let catalog = AssetAudioComponentCatalog::from_media_info(&original, fingerprint(1));
        let primary = catalog
            .components
            .iter()
            .find(|component| component.id == AudioSourceComponentId::primary())
            .expect("primary component")
            .clone();
        let replacement = media_info(vec![
            stream(1, 11, "jpn", ChannelLayout::Surround51Side, true),
            stream(7, 70, "deu", ChannelLayout::Mono, false),
        ]);

        let reconciled =
            catalog.reconcile(&replacement, fingerprint(2)).expect("reconcile catalog");

        assert_eq!(
            reconciled
                .components
                .iter()
                .find(|component| component.id == AudioSourceComponentId::primary()),
            Some(&primary)
        );
        assert!(matches!(
            reconciled.resolve(AudioSourceComponentId::primary(), &replacement),
            Err(AudioComponentCatalogError::StreamBindingDrift { .. })
        ));
        assert_eq!(reconciled.components.len(), 3);
        assert!(reconciled
            .components
            .iter()
            .any(|component| component.binding.stream_index == 7));
    }

    #[test]
    fn explicit_rebind_preserves_identity_and_may_alias_a_discovered_stream() {
        let original = media_info(vec![
            stream(1, 10, "eng", ChannelLayout::Stereo, true),
            stream(3, 30, "fra", ChannelLayout::Stereo, false),
        ]);
        let catalog = AssetAudioComponentCatalog::from_media_info(&original, fingerprint(1));
        let replacement = media_info(vec![
            stream(1, 11, "jpn", ChannelLayout::Surround51Side, true),
            stream(7, 70, "deu", ChannelLayout::Mono, false),
        ]);
        let reconciled = catalog.reconcile(&replacement, fingerprint(2)).expect("reconcile");
        let discovered_id = reconciled
            .components
            .iter()
            .find(|component| component.binding.stream_index == 7)
            .expect("newly discovered stream")
            .id;

        let rebound = reconciled
            .rebind(
                AudioSourceComponentId::primary(),
                7,
                &replacement,
                fingerprint(2),
            )
            .expect("explicit rebind");

        assert_eq!(
            rebound
                .resolve(AudioSourceComponentId::primary(), &replacement)
                .expect("rebound primary")
                .index,
            7
        );
        assert_eq!(
            rebound
                .resolve(discovered_id, &replacement)
                .expect("existing discovered alias")
                .index,
            7
        );
        assert_eq!(rebound.components.len(), reconciled.components.len());
    }

    #[test]
    fn explicit_rebind_rejects_unknown_logical_or_physical_identity() {
        let info = media_info(vec![stream(2, 20, "eng", ChannelLayout::Stereo, true)]);
        let catalog = AssetAudioComponentCatalog::from_media_info(&info, fingerprint(1));

        assert!(matches!(
            catalog.rebind(AudioSourceComponentId::new(), 2, &info, fingerprint(1)),
            Err(AudioComponentCatalogError::UnknownComponent { .. })
        ));
        assert_eq!(
            catalog.rebind(AudioSourceComponentId::primary(), 99, &info, fingerprint(1)),
            Err(AudioComponentCatalogError::UnknownPhysicalStream { stream_index: 99 })
        );
    }

    #[test]
    fn execution_selection_rejects_file_revision_drift_before_stream_binding() {
        let info = media_info(vec![stream(2, 20, "eng", ChannelLayout::Stereo, true)]);
        let catalog = AssetAudioComponentCatalog::from_media_info(&info, fingerprint(1));

        assert!(matches!(
            catalog.resolve_current_selection(
                AudioSourceComponentId::primary(),
                &info,
                fingerprint(2)
            ),
            Err(AudioComponentCatalogError::SourceRevisionDrift)
        ));
    }
}
