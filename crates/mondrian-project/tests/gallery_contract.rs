use mondrian_core::{
    AuthoringList, GalleryColorStatistics, GalleryGradeVersionBinding, GalleryRasterColorSpace,
    GalleryStill, GalleryStillId, GalleryStillRaster, ProjectColorEnvironment, ProjectSettings,
    SequenceId, TimelineTime,
};
use mondrian_project::ProjectDocument;
use mondrian_timeline::{Sequence, SequenceCollection};
use std::io::Cursor;

fn png_1x1() -> Vec<u8> {
    let image = image::RgbaImage::from_raw(1, 1, vec![32, 64, 128, 255]).expect("pixel raster");
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("encode PNG");
    png.into_inner()
}

fn grayscale_png_1x1() -> Vec<u8> {
    let image = image::GrayImage::from_raw(1, 1, vec![64]).expect("pixel raster");
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(image)
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("encode PNG");
    png.into_inner()
}

fn project_with_gallery() -> ProjectDocument {
    let mut sequence = Sequence::new("Timeline");
    let settings = sequence.settings.clone();
    let definition_id = sequence.add_grade_definition("Look");
    let version_id = sequence
        .grade_definition(definition_id)
        .expect("Grade Definition")
        .active_version;
    let sequence_id = sequence.id;
    let mut project = ProjectDocument::new(
        "Gallery",
        SequenceCollection::new(sequence),
        ProjectColorEnvironment::default(),
        settings,
        ProjectSettings::default(),
    );
    project.gallery.stills.push(GalleryStill {
        id: GalleryStillId::new(),
        name: "Reference".to_owned(),
        source_sequence_id: sequence_id,
        source_time: TimelineTime::ZERO,
        presentation_fingerprint: [5; 32],
        active_grade_versions: AuthoringList::from([GalleryGradeVersionBinding {
            definition_id,
            version_id,
        }]),
        raster: GalleryStillRaster {
            width: 1,
            height: 1,
            color_space: GalleryRasterColorSpace::Srgb,
            png: png_1x1(),
        },
        statistics: GalleryColorStatistics {
            sample_count: 1,
            low_rgb: [0.1; 3],
            median_rgb: [0.1; 3],
            high_rgb: [0.1; 3],
        },
    });
    project
}

#[test]
fn project_validation_closes_gallery_sequence_and_version_references() {
    let project = project_with_gallery();
    project.validate().expect("valid Project Gallery references");

    let mut missing_sequence = project.clone();
    missing_sequence.gallery.stills[0].source_sequence_id = SequenceId::new();
    assert!(missing_sequence.validate().is_err());

    let mut missing_version = project;
    missing_version.gallery.stills[0].active_grade_versions[0].version_id =
        mondrian_core::GradeVersionId::new();
    assert!(missing_version.validate().is_err());
}

#[test]
fn project_validation_rejects_corrupt_or_misdeclared_gallery_png() {
    let mut corrupt = project_with_gallery();
    corrupt.gallery.stills[0].raster.png.truncate(12);
    assert!(corrupt.validate().is_err());

    let mut wrong_extent = project_with_gallery();
    wrong_extent.gallery.stills[0].raster.width = 2;
    assert!(wrong_extent.validate().is_err());

    let mut noncanonical_pixels = project_with_gallery();
    noncanonical_pixels.gallery.stills[0].raster.png = grayscale_png_1x1();
    assert!(noncanonical_pixels.validate().is_err());
}
