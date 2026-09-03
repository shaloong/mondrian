//! Explicit cold/warm CPU boundary diagnostics, never performance qualification.

use std::{io::Write, time::Instant};

use mondrian_core::ocio::OcioCpuProcessorCacheDiagnostics;
use mondrian_core::{
    types::ColorSpace, ProjectColorEnvironment, WorkingColorSpace, WorkingRgbaF32Frame,
};
use mondrian_renderer::color::{MonitorColorModule, ProgramOutputModule, ProgramOutputRole};
use mondrian_renderer::{ColorFrameEncoding, CpuColorFrame, RenderCpuColorExecutionSession};
use mondrian_timeline::sequence::SequenceSettings;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn cache_facts(facts: OcioCpuProcessorCacheDiagnostics) -> Value {
    json!({
        "hits": facts.hits, "misses": facts.misses, "evictions": facts.evictions,
        "entries": facts.entries, "capacity": facts.capacity,
        "shared_static_hits": facts.shared_static_hits,
        "shared_static_misses": facts.shared_static_misses,
    })
}

fn input_digest(frame: &CpuColorFrame) -> String {
    let mut hash = Sha256::new();
    for pixel in &frame.rgba_f32().data {
        for channel in pixel {
            hash.update(channel.to_bits().to_le_bytes());
        }
    }
    format!("{:x}", hash.finalize())
}

#[test]
#[ignore = "manual diagnostic only; cold observations cannot be replaced by warm samples"]
fn cpu_output_boundary_cold_and_owner_warm_probe() -> anyhow::Result<()> {
    let setup_started = Instant::now();
    let settings = SequenceSettings::default();
    let environment = ProjectColorEnvironment::default();
    let context = settings.root_program_color_context(&environment)?;
    assert_eq!(
        context.working_color_space(),
        WorkingColorSpace::LinearRec2020
    );
    let (width, height) = (960_u32, 540_u32);
    let data = (0..width * height)
        .map(|index| {
            let x = (index % width) as f32 / (width - 1) as f32;
            let y = (index / width) as f32 / (height - 1) as f32;
            [
                -0.025 + 1.6 * x,
                0.12 + 0.9 * y,
                0.05 + 0.7 * (1.0 - x) * y,
                (index % 5) as f32 * 0.25,
            ]
        })
        .collect();
    let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
        width,
        height,
        data,
        color_space: WorkingColorSpace::LinearRec2020,
    });
    let fixture_context_setup_us = setup_started.elapsed().as_micros();
    let original_input_sha256 = input_digest(&frame);
    let mut baseline = None;
    let mut observations = Vec::new();

    // A includes the process's first CPU execution, never hidden warm-up.
    // B drops A's owner handles but leaves process-shared parent retention
    // alone. OCIO may itself cache CPU handles within those parents, so B0
    // does not isolate CPU finalization or simulate prior GPU-only warm-up.
    for (owner, count) in [("a", 6), ("b", 4)] {
        let mut session = RenderCpuColorExecutionSession::new(32);
        for index in 0..count {
            let before = session.diagnostics();
            let complete_started = Instant::now();
            let boundary_started = Instant::now();
            let boundary = ProgramOutputModule::boundary(ProgramOutputRole::Display, &context)?;
            let boundary_plan_us = boundary_started.elapsed().as_micros();
            let monitor_started = Instant::now();
            let monitor = MonitorColorModule::plan(&boundary, ColorSpace::Srgb)?;
            let monitor_plan_us = monitor_started.elapsed().as_micros();
            let execute_started = Instant::now();
            let result =
                MonitorColorModule::present_cpu_rgba8(&frame, &boundary, &monitor, &mut session)?;
            let execute_us = execute_started.elapsed().as_micros();
            let complete_boundary_us = complete_started.elapsed().as_micros();
            let after = session.diagnostics();

            // Hashing, serialization, and assertions are outside the timers.
            assert_eq!(boundary.output_color_space(), ColorSpace::Rec709);
            assert!(boundary.tone_map() && boundary.ocio_display_view().is_some());
            assert!(monitor.requires_pass());
            assert_eq!(result.stage_diagnostics.total_stages, 2);
            assert_eq!(result.output_descriptor.width, width);
            assert_eq!(result.output_descriptor.height, height);
            assert_eq!(
                result.output_descriptor.color_space.color(),
                Some(ColorSpace::Srgb)
            );
            assert_eq!(
                result.output_descriptor.encoding,
                ColorFrameEncoding::EncodedRgba8
            );
            assert_eq!(result.rgba.len(), width as usize * height as usize * 4);
            for (source, output) in frame.rgba_f32().data.iter().zip(result.rgba.chunks_exact(4)) {
                assert_eq!(output[3], (source[3].clamp(0.0, 1.0) * 255.0).round() as u8);
            }
            if let Some(expected) = &baseline {
                assert!(
                    &result.rgba == expected,
                    "RGBA parity differs for {owner}{index}"
                );
            }
            observations.push(json!({
                "owner": owner, "index": index,
                "boundary_plan_us": boundary_plan_us,
                "monitor_plan_us": monitor_plan_us,
                "execute_us": execute_us,
                "complete_boundary_us": complete_boundary_us,
                "session_before": cache_facts(before),
                "session_after": cache_facts(after),
                "session_delta": {
                    "hits": after.hits - before.hits,
                    "misses": after.misses - before.misses,
                    "evictions": after.evictions - before.evictions,
                    "shared_static_hits": after.shared_static_hits - before.shared_static_hits,
                    "shared_static_misses": after.shared_static_misses - before.shared_static_misses,
                },
                "rgba_sha256": format!("{:x}", Sha256::digest(&result.rgba)),
                "alpha_parity": true, "rgba_parity": true,
            }));
            if baseline.is_none() {
                baseline = Some(result.rgba);
            }
        }
    }
    // Separately timed public stages run only after the cold/owner observations.
    // Alternate retained and consuming monitor inputs to expose the cost of
    // retaining an otherwise unused Program Output raster in presentation.
    let boundary = ProgramOutputModule::boundary(ProgramOutputRole::Display, &context)?;
    let monitor = MonitorColorModule::plan(&boundary, ColorSpace::Srgb)?;
    let mut session = RenderCpuColorExecutionSession::new(32);
    let mut stage_observations = Vec::new();
    for index in 0..6 {
        for retain_program in [true, false] {
            let started = Instant::now();
            let program = ProgramOutputModule::execute_cpu_float(&frame, &boundary, &mut session)?;
            let program_us = started.elapsed().as_micros();
            let retained = retain_program.then(|| program.frame.clone());
            let started = Instant::now();
            let monitor = mondrian_renderer::CpuColorTransformExecutor::monitor_adaptation_float_owned_with_session(
                program.frame, &monitor, &mut session,
            )?;
            let monitor_us = started.elapsed().as_micros();
            let started = Instant::now();
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            for pixel in &monitor.frame.rgba_f32().data {
                for channel in pixel {
                    rgba.push((channel.clamp(0.0, 1.0) * 255.0).round() as u8);
                }
            }
            let reference_scalar_quantize_us = started.elapsed().as_micros();
            assert!(baseline.as_ref() == Some(&rgba), "split-stage RGBA parity");
            stage_observations.push(json!({
                "index": index, "retain_program": retain_program,
                "program_us": program_us, "monitor_us": monitor_us,
                "reference_scalar_quantize_us": reference_scalar_quantize_us,
            }));
            drop(retained);
        }
    }
    assert_eq!(input_digest(&frame), original_input_sha256);
    let report = json!({
        "schema_version": 2, "diagnostic_only": true, "qualifying": false,
        "width": width, "height": height,
        "fixture_context_setup_us": fixture_context_setup_us,
        "input_sha256": original_input_sha256, "input_unchanged": true,
        "observations": observations,
        "warm_stage_observations": stage_observations,
    });
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_CPU_OUTPUT_PROBE={report_json}");
    if let Some(path) = std::env::var_os("MONDRIAN_CPU_OUTPUT_PROBE_REPORT") {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(report_json.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    Ok(())
}
