use mondrian_core::types::{ColorEngine, ColorSpace};
use mondrian_core::{ProjectColorEnvironment, WorkingColorSpace};
use mondrian_renderer::color::{
    MonitorColorModule, ProgramOutputModule, ProgramOutputRole, SourceColorModule,
    WorkingColorModule,
};
use mondrian_renderer::{
    CpuEncodedColorFrame, CpuSourceColorFrame, RenderCpuColorExecutionSession,
};
use mondrian_timeline::sequence::{NestedColorProcessing, SequenceSettings};

#[test]
fn four_color_modules_form_one_closed_cpu_product_path() {
    let settings = SequenceSettings::default();
    let context = settings
        .root_program_color_context(&ProjectColorEnvironment::default())
        .expect("default Program color context");
    let media_context = context.media_input(false);
    let source = CpuSourceColorFrame::from(CpuEncodedColorFrame::source_rgba8(
        2,
        1,
        ColorSpace::Rec709,
        vec![32, 96, 192, 255, 220, 80, 16, 128],
    ));
    let mut session = RenderCpuColorExecutionSession::new(8);

    let source_output = SourceColorModule::execute_cpu(&source, &media_context, &mut session)
        .expect("source Module");
    assert_eq!(
        source_output.frame().descriptor().color_space.working(),
        Some(context.working_color_space())
    );
    assert_eq!(source_output.stage_diagnostics().cpu_input_stages, 1);

    let converted = WorkingColorModule::execute_cpu(
        source_output.frame(),
        WorkingColorSpace::LinearRec709,
        ColorEngine::mondrian_standard(),
        &mut session,
    )
    .expect("working Module");
    assert_eq!(
        converted.frame().descriptor().color_space.working(),
        Some(WorkingColorSpace::LinearRec709)
    );

    let boundary = ProgramOutputModule::boundary(ProgramOutputRole::Display, &context)
        .expect("Program Output boundary");
    let program =
        ProgramOutputModule::execute_cpu_float(source_output.frame(), &boundary, &mut session)
            .expect("Program Output Module");
    assert_eq!(
        program.output_descriptor.color_space.color(),
        Some(ColorSpace::Rec709)
    );

    let monitor = MonitorColorModule::plan(&boundary, ColorSpace::Srgb).expect("monitor plan");
    let presented = MonitorColorModule::present_cpu_rgba8(
        source_output.frame(),
        &boundary,
        &monitor,
        &mut session,
    )
    .expect("monitor Module");
    assert_eq!(presented.rgba.len(), 8);
    assert_eq!(
        presented.output_descriptor.color_space.color(),
        Some(ColorSpace::Srgb)
    );
    assert_eq!(presented.stage_diagnostics.total_stages, 2);

    let diagnostics = session.diagnostics();
    assert!(diagnostics.entries <= diagnostics.capacity);
}

#[test]
fn program_output_boundary_rejects_nested_working_context() {
    let settings = SequenceSettings::default();
    let root = settings
        .root_program_color_context(&ProjectColorEnvironment::default())
        .expect("root context");
    let nested = settings
        .nested_render_color_context(&root, NestedColorProcessing::PreserveChildWorkingSpace)
        .expect("nested context");

    let error = ProgramOutputModule::boundary(ProgramOutputRole::Display, &nested)
        .expect_err("working-only nested context must not become Program Output");
    assert!(error.to_string().contains("working space"));
}
