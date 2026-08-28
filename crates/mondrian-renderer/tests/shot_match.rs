use mondrian_core::{GalleryColorStatistics, WorkingColorSpace, WorkingRgbaF32Frame};
use mondrian_renderer::{analyze_shot_match_frame, solve_shot_match, CpuColorFrame};

fn frame(data: Vec<[f32; 4]>) -> CpuColorFrame {
    CpuColorFrame::working(WorkingRgbaF32Frame {
        width: u32::try_from(data.len()).expect("fixture width"),
        height: 1,
        data,
        color_space: WorkingColorSpace::LinearRec709,
    })
}

#[test]
fn analysis_is_deterministic_and_ignores_transparent_non_finite_samples() {
    let frame = frame(vec![
        [0.1, 0.2, 0.3, 1.0],
        [0.4, 0.5, 0.6, 1.0],
        [100.0, 100.0, 100.0, 0.0],
        [f32::NAN, 0.0, 0.0, 1.0],
    ]);
    let first = analyze_shot_match_frame(&frame).expect("statistics");
    let second = analyze_shot_match_frame(&frame).expect("statistics");
    assert_eq!(first, second);
    assert_eq!(first.sample_count, 2);
    assert_eq!(first.low_rgb, [0.1, 0.2, 0.3]);
    assert_eq!(first.high_rgb, [0.4, 0.5, 0.6]);
}

#[test]
fn solution_maps_target_span_and_median_with_bounded_parameters() {
    let reference = GalleryColorStatistics {
        sample_count: 10,
        low_rgb: [0.1, 0.2, 0.3],
        median_rgb: [0.5, 0.6, 0.7],
        high_rgb: [0.9, 1.0, 1.1],
    };
    let target = GalleryColorStatistics {
        sample_count: 10,
        low_rgb: [0.0, 0.1, 0.2],
        median_rgb: [0.2, 0.3, 0.4],
        high_rgb: [0.4, 0.5, 0.6],
    };
    let solution = solve_shot_match(&reference, &target);
    for gain in solution.gain_rgb {
        assert!((gain - 2.0).abs() < 1.0e-6);
    }
    assert!((solution.offset_rgb[0] - 0.1).abs() < 1.0e-6);
    assert_eq!(solution.offset_rgb[1], 0.0);
    assert!((solution.offset_rgb[2] + 0.1).abs() < 1.0e-6);
}
