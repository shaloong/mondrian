//! Kernel throughput report.
//!
//! Prints milliseconds and megapixels per second for the spatial and point
//! kernels so performance claims are measured instead of assumed. Run with
//! `cargo test -p mondrian-effects --lib report_kernel_throughput -- --nocapture`.

use std::hint::black_box;
use std::time::{Duration, Instant};

use crate::black_white::BlackAndWhiteGrade;
use crate::box_blur::{box_blur_frame_f32, BoxBlurDirection};
use crate::bulge::bulge_frame_f32;
use crate::bulge::BulgeGrade;
use crate::channel_mixer::ChannelMixerGrade;
use crate::convolution::convolution_frame_f32;
use crate::convolution::ConvolutionGrade;
use crate::drop_shadow::DropShadowGrade;
use crate::hue_saturation::HueSaturationGrade;
use crate::lens_flare::lens_flare_frame_f32;
use crate::lens_flare::{LensFlareGrade, LensFlareType};
use crate::mirror::mirror_frame_f32;
use crate::mirror::MirrorGrade;
use crate::noise::noise_frame_f32;
use crate::noise::{NoiseGrade, NoiseType};
use crate::offset::offset_frame_f32;
use crate::offset::OffsetGrade;
use crate::polar_coordinates::polar_coordinates_frame_f32;
use crate::polar_coordinates::{PolarConversion, PolarCoordinatesGrade, PolarInterpolation};
use crate::radial_blur::radial_blur_frame_f32;
use crate::radial_blur::{RadialBlurGrade, RadialBlurMode};
use crate::shift_channels::{ChannelSource, ShiftChannelsGrade};
use crate::spherize::spherize_frame_f32;
use crate::spherize::SpherizeGrade;
use crate::split::split_frame_f32;
use crate::split::SplitGrade;
use crate::twirl::twirl_frame_f32;
use crate::twirl::TwirlGrade;
use crate::{HueBand, InvertChannels};
use mondrian_core::Color;

const WIDTH: usize = 1280;
const HEIGHT: usize = 720;
const MEGAPIXELS: f64 = (WIDTH * HEIGHT) as f64 / 1_000_000.0;

fn frame() -> Vec<[f32; 4]> {
    (0..WIDTH * HEIGHT)
        .map(|index| {
            let t = (index % 97) as f32 / 96.0;
            [t, 1.0 - t, 0.25 + 0.5 * t, 1.0]
        })
        .collect()
}

fn report(name: &str, elapsed: Duration) {
    let millis = elapsed.as_secs_f64() * 1_000.0;
    let throughput = MEGAPIXELS / elapsed.as_secs_f64();
    // 4K UHD is 8.29 megapixels; report what that budget implies.
    let projected = millis * (8.29 / MEGAPIXELS);
    println!("{name:<44} {millis:>8.2} ms  {throughput:>8.1} MP/s  (4K: {projected:>8.1} ms)");
}

fn time<T>(name: &str, mut run: impl FnMut() -> T) {
    let start = Instant::now();
    black_box(run());
    report(name, start.elapsed());
}

#[test]
fn report_kernel_throughput() {
    println!(
        "\n== mondrian-effects kernel throughput ({WIDTH}x{HEIGHT}, Windows, release-ish debug) =="
    );

    let source = frame();
    let mut buffer = source.clone();
    time("box blur r=50 x3 passes (horizontal+vertical)", || {
        buffer.copy_from_slice(&source);
        box_blur_frame_f32(
            &mut buffer,
            WIDTH,
            HEIGHT,
            50.0,
            3,
            BoxBlurDirection::HorizontalAndVertical,
            true,
        )
    });

    let mut buffer = source.clone();
    time("box blur r=30000 x3 passes (half frame radius)", || {
        buffer.copy_from_slice(&source);
        box_blur_frame_f32(
            &mut buffer,
            WIDTH,
            HEIGHT,
            30_000.0,
            3,
            BoxBlurDirection::HorizontalAndVertical,
            true,
        )
    });

    let mut buffer = source.clone();
    time("directional blur length=100 (101 taps)", || {
        buffer.copy_from_slice(&source);
        crate::directional_blur::apply_directional_blur_f32_controlled(
            &mut buffer,
            WIDTH,
            HEIGHT,
            100.0,
            30.0,
            &mut || Ok::<(), std::convert::Infallible>(()),
        )
        .expect("infallible checkpoint")
    });

    let grade = DropShadowGrade::new(
        Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 },
        0.5,
        135.0,
        20.0,
        40.0,
        false,
    )
    .expect("valid drop shadow");
    let mut buffer = source.clone();
    time("drop shadow (distance 20, softness 40)", || {
        buffer.copy_from_slice(&source);
        crate::drop_shadow::apply_drop_shadow_f32_controlled(
            &mut buffer,
            WIDTH,
            HEIGHT,
            grade,
            &mut || Ok::<(), std::convert::Infallible>(()),
        )
        .expect("infallible checkpoint")
    });

    let invert = InvertChannels::Hls;
    let mut buffer = source.clone();
    time("point: invert HLS", || {
        for pixel in buffer.iter_mut() {
            *pixel = crate::invert_rgba_f32(*pixel, invert, 0.0);
        }
    });

    let hue = HueSaturationGrade::new(HueBand::Reds, 45.0, 30.0, -10.0, 0.25, 150.0, 40.0, 5.0)
        .expect("valid hue grade");
    let mut buffer = source.clone();
    time("point: hue/saturation (banded + colorize)", || {
        for pixel in buffer.iter_mut() {
            let graded = hue.apply([pixel[0], pixel[1], pixel[2]]);
            pixel[0] = graded[0];
            pixel[1] = graded[1];
            pixel[2] = graded[2];
        }
    });

    let mono = BlackAndWhiteGrade::new([40.0, 60.0, 40.0, 60.0, 20.0, 80.0], 0.35, [0.9, 0.8, 0.6])
        .expect("valid black and white");
    let mut buffer = source.clone();
    time("point: black and white (tinted)", || {
        for pixel in buffer.iter_mut() {
            let graded = mono.apply([pixel[0], pixel[1], pixel[2]]);
            pixel[0] = graded[0];
            pixel[1] = graded[1];
            pixel[2] = graded[2];
        }
    });

    let mixer = ChannelMixerGrade::new(
        [
            90.0, 20.0, 0.0, 0.0, 0.0, 110.0, 0.0, -10.0, 0.0, 0.0, 100.0, 5.0,
        ],
        false,
    )
    .expect("valid channel mixer");
    let mut buffer = source.clone();
    time("point: channel mixer (12 weights)", || {
        for pixel in buffer.iter_mut() {
            let graded = mixer.apply([pixel[0], pixel[1], pixel[2]]);
            pixel[0] = graded[0];
            pixel[1] = graded[1];
            pixel[2] = graded[2];
        }
    });

    let channels = ShiftChannelsGrade::new(
        ChannelSource::Green,
        ChannelSource::Luminance,
        ChannelSource::Blue,
        ChannelSource::Alpha,
        [0.2126, 0.7152, 0.0722],
    );
    let mut buffer = source.clone();
    time("point: shift channels (luminance source)", || {
        for pixel in buffer.iter_mut() {
            *pixel = channels.apply(*pixel);
        }
    });

    // Resampling kernels read a source snapshot, so they are the ones whose cost
    // scales with taps and with cache behaviour instead of with the pixel count alone.
    let twirl = TwirlGrade::new(160.0, 0.45, [640.0, 360.0]).expect("valid twirl");
    let mut buffer = source.clone();
    time("remap: twirl 160 deg / 45% radius", || {
        buffer.copy_from_slice(&source);
        twirl_frame_f32(&mut buffer, WIDTH, HEIGHT, twirl)
    });

    let spherize = SpherizeGrade::new(220.0, [640.0, 360.0]).expect("valid spherize");
    let mut buffer = source.clone();
    time("remap: spherize r=220", || {
        buffer.copy_from_slice(&source);
        spherize_frame_f32(&mut buffer, WIDTH, HEIGHT, spherize)
    });

    let bulge = BulgeGrade::new([240.0, 180.0], [640.0, 360.0], 1.0, 0.0, true, false)
        .expect("valid bulge");
    let mut buffer = source.clone();
    time("remap: bulge 240x180 antialiased", || {
        buffer.copy_from_slice(&source);
        bulge_frame_f32(&mut buffer, WIDTH, HEIGHT, bulge)
    });

    let spin = RadialBlurGrade::new(400.0, [640.0, 360.0], RadialBlurMode::Spin, true, 7)
        .expect("valid radial blur");
    let mut buffer = source.clone();
    time("remap: radial blur spin amount=400", || {
        buffer.copy_from_slice(&source);
        radial_blur_frame_f32(&mut buffer, WIDTH, HEIGHT, spin)
    });

    let zoom = RadialBlurGrade::new(400.0, [640.0, 360.0], RadialBlurMode::Zoom, true, 7)
        .expect("valid radial blur");
    let mut buffer = source.clone();
    time("remap: radial blur zoom amount=400", || {
        buffer.copy_from_slice(&source);
        radial_blur_frame_f32(&mut buffer, WIDTH, HEIGHT, zoom)
    });

    let mirror = MirrorGrade::new(37.5, [640.0, 360.0]).expect("valid mirror");
    let mut buffer = source.clone();
    time("remap: mirror 37.5 deg", || {
        buffer.copy_from_slice(&source);
        mirror_frame_f32(&mut buffer, WIDTH, HEIGHT, mirror)
    });

    let offset = OffsetGrade::new([13.5, -7.25], 0.0).expect("valid offset");
    let mut buffer = source.clone();
    time("remap: offset 13.5,-7.25 px", || {
        buffer.copy_from_slice(&source);
        offset_frame_f32(&mut buffer, WIDTH, HEIGHT, offset)
    });

    let polar =
        PolarCoordinatesGrade::new(PolarConversion::RectToPolar, PolarInterpolation::Bilinear);
    let mut buffer = source.clone();
    time("remap: polar rect to polar (bilinear)", || {
        buffer.copy_from_slice(&source);
        polar_coordinates_frame_f32(&mut buffer, WIDTH, HEIGHT, polar)
    });

    let split = SplitGrade::new([100.0, 100.0], [1100.0, 620.0], 24.0).expect("valid split");
    let mut buffer = source.clone();
    time("remap: split 24 px", || {
        buffer.copy_from_slice(&source);
        split_frame_f32(&mut buffer, WIDTH, HEIGHT, split)
    });

    let flare = LensFlareGrade::new(150.0, 0.0, [640.0, 360.0], LensFlareType::Zoom50To300)
        .expect("valid lens flare");
    let mut buffer = source.clone();
    time("shade: lens flare zoom 50-300", || {
        buffer.copy_from_slice(&source);
        lens_flare_frame_f32(&mut buffer, WIDTH, HEIGHT, flare)
    });

    let convolution = ConvolutionGrade::new(
        [0.0, -1.0, 0.0, -1.0, 5.0, -1.0, 0.0, -1.0, 0.0],
        false,
        0.0,
    )
    .expect("valid convolution");
    let mut buffer = source.clone();
    time("neighbour: convolution sharpen 3x3", || {
        buffer.copy_from_slice(&source);
        convolution_frame_f32(&mut buffer, WIDTH, HEIGHT, convolution)
    });

    let noise = NoiseGrade::new(0.5, NoiseType::Rgb, false).expect("valid noise");
    let mut buffer = source.clone();
    time("point: noise rgb amount=50%", || {
        buffer.copy_from_slice(&source);
        noise_frame_f32(&mut buffer, WIDTH, HEIGHT, noise)
    });
}
