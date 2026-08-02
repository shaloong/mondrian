use glam::{Vec2, Vec3};
use mondrian_core::{
    automation::{ParameterResourceReference, PropertyDescriptor, PropertyValue},
    BlendMode, Color, TimelineTime, WorkingColorSpace,
};
use mondrian_effects::{
    EffectCachePolicy, EffectColorDomainContract, EffectDeterminism, EffectEvalContext,
    EffectExecutionContract, EffectExecutionModes, EffectGraphDsl, EffectGraphTopology,
    EffectGraphValue, EffectPluginDefinitionBuilder, EffectRenderOp, EffectResourceLifetime,
    EffectRoiPropagation, EffectStateModel, EffectTemporalInputExtent, EffectType, Lut3D, MaskOp,
    PreparedLut3D,
};
use std::sync::Arc;

fn current_frame_contract(
    execution_modes: EffectExecutionModes,
    topology: EffectGraphTopology,
) -> EffectExecutionContract {
    EffectExecutionContract {
        execution_modes,
        determinism: EffectDeterminism::Deterministic,
        state_model: EffectStateModel::Stateless,
        temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
        roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
        resource_lifetime: EffectResourceLifetime::Frame,
        topology,
    }
}

fn compile_branching_dsl_signatures(
    graph: &mut EffectGraphDsl<'_>,
    input: EffectGraphValue,
    alpha_mask_input: EffectGraphValue,
) {
    let branch = graph.branch(input, |graph, source| {
        graph.apply_to(source, EffectRenderOp::GaussianBlur { radius: 2.0 })
    });
    let blended = graph.blend(branch, BlendMode::Screen, 0.5, |graph, source| {
        graph.apply_to(source, EffectRenderOp::Sharpen { amount: 0.25 })
    });
    let masked = graph.mask(blended, false, MaskOp::Intersect, |_graph, _source| {
        alpha_mask_input
    });
    graph.set_output(masked);
}

#[test]
fn public_plugin_sdk_contract_compiles_from_crate_reexports() {
    let plugin_type = EffectType::Plugin("plugin.test.public_api".to_owned());
    let amount_id = plugin_type.parameter_id("amount").expect("static parameter ID");
    let amount_id_for_graph = amount_id.clone();

    let definition = EffectPluginDefinitionBuilder::new(
        plugin_type.key(),
        "Public API",
        EffectColorDomainContract::SCENE_LINEAR,
    )
    .with_execution_contract(current_frame_contract(
        EffectExecutionModes::CPU_F32,
        EffectGraphTopology::GeneralDag,
    ))
    .property(
        PropertyDescriptor::new(
            "plugin.test.public_api.amount",
            "Amount",
            PropertyValue::Float(0.5),
        )
        .with_parameter_id(amount_id),
    )
    .with_branching_graph(move |effect, context, graph| {
        let amount = effect.evaluate_f32_parameter(&amount_id_for_graph, context.time, 0.5);
        let adjusted = graph.apply(EffectRenderOp::ColorAdjust {
            exposure: amount,
            contrast: 1.0,
            saturation: 1.0,
            working_color_space: context.working_color_space,
        });
        let blended = graph.blend(adjusted, BlendMode::Screen, 0.25, |graph, source| {
            graph.apply_to(source, EffectRenderOp::GaussianBlur { radius: 2.0 })
        });
        graph.set_output(blended);
    })
    .build();

    assert_eq!(definition.key(), "plugin.test.public_api");
    assert_eq!(
        definition.execution_contract().topology,
        EffectGraphTopology::GeneralDag
    );

    let custom = EffectPluginDefinitionBuilder::new(
        "plugin.test.public_api.custom",
        "Public Custom API",
        EffectColorDomainContract::SCENE_LINEAR,
    )
    .with_execution_contract(current_frame_contract(
        EffectExecutionModes::CPU_U8,
        EffectGraphTopology::LinearChain,
    ))
    .with_custom_render_backend(
        Arc::new(|_, _| Ok(Some(serde_json::json!({ "amount": 0.5 })))),
        None,
        EffectCachePolicy::Deterministic,
        Arc::new(|_, _, _, _, _| Ok(())),
    )
    .build();
    assert_eq!(
        custom.execution_contract().execution_modes,
        EffectExecutionModes::CPU_U8
    );

    let context = EffectEvalContext {
        time: TimelineTime::ZERO,
        working_color_space: WorkingColorSpace::LinearRec709,
    };
    assert_eq!(context.working_color_space, WorkingColorSpace::LinearRec709);

    let _property_values = [
        PropertyValue::Bool(true),
        PropertyValue::Int(1_i64),
        PropertyValue::Float(1.0_f32),
        PropertyValue::Double(1.0_f64),
        PropertyValue::Vec2(Vec2::ONE),
        PropertyValue::Vec3(Vec3::ONE),
        PropertyValue::Color(Color::WHITE),
        PropertyValue::Vec4([1.0; 4]),
        PropertyValue::Enum("option".to_owned()),
        PropertyValue::Resource(ParameterResourceReference::Unbound),
        PropertyValue::Text("text".to_owned()),
    ];

    let sample_offset = TimelineTime::new(-1, 24).expect("valid temporal offset");
    let _temporal = EffectRenderOp::TemporalFrameBlend { sample_offset, mix: 0.5 };
    let lut = Lut3D::identity(2).expect("identity LUT");
    let _lut = EffectRenderOp::Lut3D {
        lut: Arc::new(PreparedLut3D::new(lut)),
        intensity: 1.0,
    };

    let _dsl_signature: fn(&mut EffectGraphDsl<'_>, EffectGraphValue, EffectGraphValue) =
        compile_branching_dsl_signatures;
}
