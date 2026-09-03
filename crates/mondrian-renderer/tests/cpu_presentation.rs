//! Public Viewer presentation parity and full-float-raster allocation regression.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

use mondrian_core::{
    apply_signal_monitoring_rgba_f32, types::ColorSpace, ProgramScopesTap, ProjectColorEnvironment,
    SignalComplianceContract, SignalMonitoringSettings, WorkingColorSpace, WorkingRgbaF32Frame,
};
use mondrian_renderer::{
    color::{MonitorColorModule, ProgramOutputModule, ProgramOutputRole},
    CpuColorFrame, CpuColorTransformExecutor, RenderCpuColorExecutionSession,
};
use mondrian_timeline::sequence::SequenceSettings;

thread_local! {
    // Const TLS avoids allocating while the allocator records this thread only.
    static RASTER_ALLOCS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

struct RasterTrackingAllocator;

fn record(size: usize) {
    let _ = RASTER_ALLOCS.try_with(|state| {
        let (target, count) = state.get();
        if target != 0 && target == size {
            state.set((target, count + 1));
        }
    });
}

// SAFETY: all memory operations forward the original arguments unchanged to
// System. The recorder only touches this thread's non-allocating integer cells.
unsafe impl GlobalAlloc for RasterTrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: RasterTrackingAllocator = RasterTrackingAllocator;

fn count_rasters<T>(bytes: usize, execute: impl FnOnce() -> T) -> (T, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            RASTER_ALLOCS.with(|state| state.set((0, 0)));
        }
    }
    RASTER_ALLOCS.with(|state| state.set((bytes, 0)));
    let reset = Reset;
    let result = execute();
    let count = RASTER_ALLOCS.with(|state| state.get().1);
    drop(reset);
    (result, count)
}

#[test]
fn presentation_moves_float_raster_unless_the_selected_signal_tap_needs_it() -> anyhow::Result<()> {
    let (width, height) = (257, 33);
    let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
        width,
        height,
        color_space: WorkingColorSpace::LinearRec2020,
        data: (0..width * height)
            .map(|index| {
                let x = (index % width) as f32 / width as f32;
                [x * 1.8 - 0.05, 0.3, 1.0 - x, (index % 5) as f32 * 0.25]
            })
            .collect(),
    });
    let original = frame.rgba_f32().data.clone();
    let context = SequenceSettings::default()
        .root_program_color_context(&ProjectColorEnvironment::default())?;
    let boundary = ProgramOutputModule::boundary(ProgramOutputRole::Display, &context)?;
    let mut session = RenderCpuColorExecutionSession::new(32);
    // Exercise the other production caller of the shared quantization kernel:
    // ordinary Program Output, without any Viewer/monitor interpretation.
    let program_float = ProgramOutputModule::execute_cpu_float(&frame, &boundary, &mut session)?;
    let program_bytes = ProgramOutputModule::execute_cpu_rgba8(&frame, &boundary, &mut session)?;
    let expected_program: Vec<u8> = program_float
        .frame
        .rgba_f32()
        .data
        .iter()
        .flat_map(|pixel| {
            pixel.iter().map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8)
        })
        .collect();
    assert_eq!(program_bytes.rgba, expected_program);
    assert_eq!(
        program_bytes.output_descriptor.color_space,
        program_float.frame.descriptor().color_space
    );
    drop(program_float);
    for monitor_space in [ColorSpace::Srgb, boundary.output_color_space()] {
        let plan = MonitorColorModule::plan(&boundary, monitor_space)?;
        // Warm owner/engine resources outside allocation measurements. This is
        // a deterministic raster-lifetime test, not cold performance evidence.
        MonitorColorModule::present_cpu_rgba8(&frame, &boundary, &plan, &mut session)?;
        for tap in [
            None,
            Some(ProgramScopesTap::ProgramOutput),
            Some(ProgramScopesTap::MonitorOutput),
        ] {
            let settings = SignalMonitoringSettings { false_color: true, ..Default::default() };
            let program = ProgramOutputModule::execute_cpu_float(&frame, &boundary, &mut session)?;
            let monitor = if plan.requires_pass() {
                CpuColorTransformExecutor::monitor_adaptation_float_with_session(
                    &program.frame,
                    &plan,
                    &mut session,
                )?
                .frame
            } else {
                program.frame.clone()
            };
            let mut expected = monitor.rgba_f32().data.clone();
            if let Some(tap) = tap {
                let (signal, space) = match tap {
                    ProgramScopesTap::ProgramOutput => {
                        (&program.frame, boundary.output_color_space())
                    }
                    ProgramScopesTap::MonitorOutput => (&monitor, monitor_space),
                };
                apply_signal_monitoring_rgba_f32(
                    &signal.rgba_f32().data,
                    &mut expected,
                    width,
                    SignalComplianceContract::normalized_rgb(space)?,
                    settings,
                )?;
            }
            let expected: Vec<u8> = expected
                .iter()
                .flat_map(|pixel| {
                    pixel.iter().map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8)
                })
                .collect();
            let (result, allocations) =
                count_rasters(width as usize * height as usize * 16, || match tap {
                    None => MonitorColorModule::present_cpu_rgba8(
                        &frame,
                        &boundary,
                        &plan,
                        &mut session,
                    )
                    .map_err(anyhow::Error::from),
                    Some(tap) => MonitorColorModule::present_cpu_rgba8_with_signal_monitoring(
                        &frame,
                        &boundary,
                        &plan,
                        tap,
                        settings,
                        &mut session,
                    )
                    .map_err(anyhow::Error::from),
                });
            let result = result?;
            assert!(
                result.rgba == expected,
                "pixel parity for {tap:?}/{monitor_space:?}"
            );
            assert_eq!(result.program_output_descriptor, program.output_descriptor);
            assert_eq!(
                result.output_descriptor.color_space.color(),
                Some(monitor_space)
            );
            assert_eq!(
                result.stage_diagnostics.total_stages,
                1 + u64::from(plan.requires_pass())
            );
            assert_eq!(
                allocations,
                if tap.is_some() { 2 } else { 1 },
                "float rasters for {tap:?}/{monitor_space:?}"
            );
            assert_eq!(
                frame.rgba_f32().data,
                original,
                "borrowed working input must not change"
            );
        }
    }
    Ok(())
}
