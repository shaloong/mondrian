use mondrian_core::{
    AuthoringList, GalleryColorStatistics, GalleryGradeVersionBinding, GalleryRasterColorSpace,
    GalleryStill, GalleryStillId, GalleryStillRaster, GradeDefinitionId, GradeVersionId,
    ProjectGallery, SequenceId, TimelineTime,
};

fn png_header() -> Vec<u8> {
    b"\x89PNG\r\n\x1a\nfixture".to_vec()
}

fn statistics() -> GalleryColorStatistics {
    GalleryColorStatistics {
        sample_count: 8,
        low_rgb: [0.0, 0.1, 0.2],
        median_rgb: [0.3, 0.4, 0.5],
        high_rgb: [0.7, 0.8, 0.9],
    }
}

fn still() -> GalleryStill {
    GalleryStill {
        id: GalleryStillId::new(),
        name: "Reference".to_owned(),
        source_sequence_id: SequenceId::new(),
        source_time: TimelineTime::ZERO,
        presentation_fingerprint: [3; 32],
        active_grade_versions: AuthoringList::from([GalleryGradeVersionBinding {
            definition_id: GradeDefinitionId::new(),
            version_id: GradeVersionId::new(),
        }]),
        raster: GalleryStillRaster {
            width: 1,
            height: 1,
            color_space: GalleryRasterColorSpace::Srgb,
            png: png_header(),
        },
        statistics: statistics(),
    }
}

#[test]
fn gallery_png_serializes_as_compact_base64_and_round_trips() {
    let gallery = ProjectGallery { stills: AuthoringList::from([still()]) };
    gallery.validate().expect("valid Gallery");
    let json = serde_json::to_string(&gallery).expect("serialize Gallery");
    assert!(json.contains("iVBOR"));
    assert!(!json.contains("137,80,78,71"));
    let decoded: ProjectGallery = serde_json::from_str(&json).expect("deserialize Gallery");
    assert_eq!(decoded, gallery);
}

#[test]
fn gallery_rejects_duplicate_ids_and_unordered_statistics() {
    let first = still();
    let mut second = still();
    second.id = first.id;
    let duplicate = ProjectGallery { stills: AuthoringList::from([first, second]) };
    assert!(duplicate.validate().is_err());

    let mut invalid = statistics();
    invalid.low_rgb[1] = 0.9;
    assert!(invalid.validate().is_err());

    let mut oversized_decode = still();
    oversized_decode.raster.width = 16_384;
    oversized_decode.raster.height = 16_384;
    assert!(oversized_decode.validate().is_err());
}
