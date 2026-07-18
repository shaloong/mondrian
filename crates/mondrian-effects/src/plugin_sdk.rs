use crate::{
    effect::{
        EffectCacheKeyBuilder, EffectCachePolicy, EffectDefinition, EffectEvalContext,
        EffectGraphBuilder, EffectNode, EffectRenderParamsBuilder,
    },
    graph::{EffectGraphBuilderState, EffectGraphValue},
    CustomEffectRenderProcessor, EffectPluginContract, EffectRenderOp,
};
use mondrian_core::{
    automation::{PropertyBag, PropertyDescriptor},
    types::BlendMode,
};
use std::sync::Arc;

pub struct EffectGraphDsl<'a> {
    builder: &'a mut EffectGraphBuilderState,
}

impl<'a> EffectGraphDsl<'a> {
    pub fn new(builder: &'a mut EffectGraphBuilderState) -> Self {
        Self { builder }
    }

    pub fn source(&self) -> EffectGraphValue {
        self.builder.source()
    }

    pub fn current(&self) -> EffectGraphValue {
        self.builder.current_output()
    }

    pub fn set_output(&mut self, value: EffectGraphValue) {
        self.builder.set_current_output(value);
    }

    pub fn apply(&mut self, op: EffectRenderOp) -> EffectGraphValue {
        self.builder.append_unary(op)
    }

    pub fn apply_to(&mut self, input: EffectGraphValue, op: EffectRenderOp) -> EffectGraphValue {
        self.builder.add_unary_from(input, op)
    }

    pub fn branch<F>(&mut self, input: EffectGraphValue, build: F) -> EffectGraphValue
    where
        F: for<'b> FnOnce(&mut EffectGraphDsl<'b>, EffectGraphValue) -> EffectGraphValue,
    {
        let mut branch = EffectGraphDsl::new(self.builder);
        build(&mut branch, input)
    }

    pub fn blend<F>(
        &mut self,
        base: EffectGraphValue,
        blend_mode: BlendMode,
        opacity: f32,
        build_overlay: F,
    ) -> EffectGraphValue
    where
        F: for<'b> FnOnce(&mut EffectGraphDsl<'b>, EffectGraphValue) -> EffectGraphValue,
    {
        let overlay = self.branch(base, build_overlay);
        self.builder.add_blend(base, overlay, blend_mode, opacity)
    }

    pub fn blend_current<F>(
        &mut self,
        blend_mode: BlendMode,
        opacity: f32,
        build_overlay: F,
    ) -> EffectGraphValue
    where
        F: for<'b> FnOnce(&mut EffectGraphDsl<'b>, EffectGraphValue) -> EffectGraphValue,
    {
        self.builder.blend_current_with(blend_mode, opacity, |builder, source| {
            let mut dsl = EffectGraphDsl::new(builder);
            build_overlay(&mut dsl, source)
        })
    }

    pub fn mask<F>(
        &mut self,
        input: EffectGraphValue,
        invert: bool,
        mask_op: crate::mask::MaskOp,
        build_mask: F,
    ) -> EffectGraphValue
    where
        F: for<'b> FnOnce(&mut EffectGraphDsl<'b>, EffectGraphValue) -> EffectGraphValue,
    {
        let mask = self.branch(input, build_mask);
        self.builder.add_mask(input, mask, invert, mask_op)
    }

    pub fn mask_current<F>(
        &mut self,
        invert: bool,
        mask_op: crate::mask::MaskOp,
        build_mask: F,
    ) -> EffectGraphValue
    where
        F: for<'b> FnOnce(&mut EffectGraphDsl<'b>, EffectGraphValue) -> EffectGraphValue,
    {
        self.builder.mask_current_with(invert, mask_op, |builder, source| {
            let mut dsl = EffectGraphDsl::new(builder);
            build_mask(&mut dsl, source)
        })
    }
}

pub struct EffectPluginDefinitionBuilder {
    definition: EffectDefinition,
}

impl EffectPluginDefinitionBuilder {
    /// Start a plugin definition with its required processing-domain contract.
    pub fn new(
        key: impl Into<String>,
        display_name: impl Into<String>,
        color_domain_contract: crate::EffectColorDomainContract,
    ) -> Self {
        Self {
            definition: EffectDefinition::new(
                key,
                display_name,
                PropertyBag::default(),
                color_domain_contract,
            ),
        }
    }

    pub fn property(mut self, descriptor: PropertyDescriptor) -> Self {
        self.definition = self.definition.with_property(descriptor);
        self
    }

    pub fn properties(mut self, properties: PropertyBag) -> Self {
        self.definition = self.definition.with_properties(properties);
        self
    }

    pub fn with_graph<F>(mut self, build: F) -> Self
    where
        F: for<'a> Fn(&EffectNode, EffectEvalContext, &mut EffectGraphDsl<'a>)
            + Send
            + Sync
            + 'static,
    {
        let graph_builder: EffectGraphBuilder = Arc::new(move |effect, context, builder| {
            let mut dsl = EffectGraphDsl::new(builder);
            build(effect, context, &mut dsl);
        });
        self.definition = self.definition.with_graph_builder(graph_builder);
        self
    }

    pub fn with_branching_graph<F>(mut self, build: F) -> Self
    where
        F: for<'a> Fn(&EffectNode, EffectEvalContext, &mut EffectGraphDsl<'a>)
            + Send
            + Sync
            + 'static,
    {
        let graph_builder: EffectGraphBuilder = Arc::new(move |effect, context, builder| {
            let mut dsl = EffectGraphDsl::new(builder);
            build(effect, context, &mut dsl);
        });
        self.definition = self.definition.with_branching_graph_builder(graph_builder);
        self
    }

    pub fn with_custom_render_backend(
        mut self,
        params_builder: EffectRenderParamsBuilder,
        cache_key_builder: Option<EffectCacheKeyBuilder>,
        cache_policy: EffectCachePolicy,
        processor: CustomEffectRenderProcessor,
    ) -> Self {
        self.definition = self.definition.with_custom_render_backend(
            params_builder,
            cache_key_builder,
            cache_policy,
            processor,
        );
        self
    }

    pub fn with_plugin_contract(mut self, contract: EffectPluginContract) -> Self {
        self.definition = self.definition.with_plugin_contract(contract);
        self
    }

    pub fn build(self) -> EffectDefinition {
        self.definition
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        build_effect_render_graph, effect_definition, register_effect_definition, EffectNodeExt,
        EffectType,
    };
    use mondrian_core::{
        automation::{PropertyDescriptor, PropertyValue},
        TimelineTime,
    };

    fn tt(frame: i64) -> TimelineTime {
        TimelineTime::new(frame, 25).expect("valid test time")
    }

    #[test]
    fn plugin_definition_exposes_its_explicit_color_domain_contract() {
        let domain = crate::EffectColorDomainContract::preserving(
            crate::EffectColorDomain::LogPerceptualRgb {
                color_space: mondrian_core::ColorSpace::AcesCct,
            },
        );
        let definition =
            EffectPluginDefinitionBuilder::new("plugin.sdk.log_grade", "Log Grade", domain).build();

        assert_eq!(definition.color_domain_contract(), domain);
    }

    #[test]
    fn plugin_sdk_builds_branching_graph_definition() {
        let plugin_type = EffectType::Plugin("plugin.sdk.soft_glow".to_string());
        let radius_id = plugin_type.parameter_id("radius").expect("radius ID");
        let opacity_id = plugin_type.parameter_id("opacity").expect("opacity ID");
        let definition = EffectPluginDefinitionBuilder::new(
            plugin_type.key(),
            "Soft Glow",
            crate::EffectColorDomainContract::SCENE_LINEAR,
        )
        .property(
            PropertyDescriptor::new(
                "plugin.sdk.soft_glow.radius",
                "Radius",
                PropertyValue::Float(6.0),
            )
            .with_parameter_id(radius_id.clone()),
        )
        .property(
            PropertyDescriptor::new(
                "plugin.sdk.soft_glow.opacity",
                "Opacity",
                PropertyValue::Float(0.35),
            )
            .with_parameter_id(opacity_id.clone()),
        )
        .with_branching_graph(move |effect, context, graph| {
            let radius = effect.evaluate_f32_parameter(&radius_id, context.time, 0.0);
            let opacity = effect.evaluate_f32_parameter(&opacity_id, context.time, 0.0);
            if radius <= 1.0e-4 || opacity <= 1.0e-4 {
                return;
            }
            graph.blend_current(BlendMode::Screen, opacity, |graph, source| {
                graph.apply_to(source, EffectRenderOp::GaussianBlur { radius })
            });
        })
        .build();
        register_effect_definition(definition).expect("register SDK definition");

        let effect = crate::EffectNode::with_defaults(plugin_type.clone());
        let graph = build_effect_render_graph(&[effect], tt(0));
        assert_eq!(graph.nodes.len(), 3);

        let caps = effect_definition(&plugin_type).expect("effect definition").capabilities();
        assert!(caps.supports_render_graph);
        assert!(caps.supports_branching_render_graph);
    }
}
