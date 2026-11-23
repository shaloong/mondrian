//! Asset Library metadata used by generated Clip Info titles.

use super::*;
use mondrian_core::{TitleAssetMetadata, TitleClipInfoField, TitleMetadataField};

#[derive(Default)]
pub(super) struct PreviewTitleAssets {
    author_generation: Option<u64>,
    needs_metadata: bool,
    include_frame_rates: bool,
    library_revision: Option<u64>,
    names: Option<Arc<TitleAssetMetadata>>,
}

impl PreviewTitleAssets {
    pub(super) fn names(&self) -> Option<Arc<TitleAssetMetadata>> {
        self.names.clone()
    }

    /// Refresh only for projects that have a title requesting Library facts.
    /// Returns whether retaining a previously completed plan would be stale.
    pub(super) fn refresh(&mut self, authoring: &PreviewAuthoringSnapshot<'_>) -> bool {
        let generation = authoring.author_generation();
        if self.author_generation != Some(generation) {
            self.author_generation = Some(generation);
            let mut needs_metadata = false;
            let mut include_frame_rates = false;
            for title in authoring
                .sequences()
                .iter()
                .flat_map(|sequence| &sequence.video_tracks)
                .flat_map(|track| &track.clips)
                .filter_map(|clip| clip.content.basic_title())
            {
                needs_metadata |= title
                    .clip_info()
                    .ok()
                    .flatten()
                    .is_some_and(|info| info.field.uses_asset_metadata());
                if let Ok(Some(lines)) = title.metadata_lines() {
                    for line in lines.into_iter().filter(|line| line.enabled) {
                        needs_metadata |= line.field.uses_asset_metadata();
                        include_frame_rates |= line.field
                            == TitleMetadataField::Clip(TitleClipInfoField::MediaFrameRate);
                    }
                }
                if include_frame_rates {
                    break;
                }
            }
            self.needs_metadata = needs_metadata;
            if self.include_frame_rates != include_frame_rates {
                self.include_frame_rates = include_frame_rates;
                self.library_revision = None;
            }
        }
        let (revision, names) = if self.needs_metadata {
            let library = authoring.asset_library();
            if let Ok(revision) = library.database_revision()
                && self.library_revision == Some(revision)
            {
                return false;
            }
            match library.capture_title_asset_metadata(self.include_frame_rates) {
                Ok((revision, names)) => (Some(revision), Some(Arc::new(names))),
                Err(error) => {
                    tracing::warn!(%error, "Title Asset Library snapshot unavailable");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };
        self.library_revision = revision;
        if self.names == names {
            return false;
        }
        self.names = names;
        true
    }
}
