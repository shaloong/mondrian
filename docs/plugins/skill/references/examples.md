# Complete Examples

The snippets below assume this conservative helper is in scope. Select only an
exact mode the emitted operations really implement. The high-level plugin SDK
currently admits only stateless current-frame examples here; author-selected
external resources and continuity state must remain fail-closed until their
real preparation or Session Interface is exposed.

```rust
fn current_frame_contract(
    execution_modes: EffectExecutionModes,
    topology: EffectGraphTopology,
    resource_lifetime: EffectResourceLifetime,
) -> EffectExecutionContract {
    EffectExecutionContract {
        execution_modes,
        determinism: EffectDeterminism::Deterministic,
        state_model: EffectStateModel::Stateless,
        temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
        roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
        resource_lifetime,
        topology,
    }
}
```

## Example 1: Simple blur effect

A linear chain effect with one parameter.

```rust
use mondrian_core::automation::{PropertyDescriptor, PropertyValue};
use mondrian_effects::{
    EffectPluginContract, EffectPluginDefinitionBuilder, EffectRenderOp, EffectType,
    register_effect_definition,
};

pub fn register() {
    let plugin_type = EffectType::Plugin("plugin.example.soft_blur".to_string());
    let radius_id = plugin_type
        .parameter_id("radius")
        .expect("static parameter ID");

    let definition = EffectPluginDefinitionBuilder::new(
        plugin_type.key(),
        "Soft Blur",
        EffectColorDomainContract::SCENE_LINEAR,
    )
        .with_execution_contract(current_frame_contract(
            EffectExecutionModes::CPU_F32,
            EffectGraphTopology::LinearChain,
            EffectResourceLifetime::Frame,
        ))
        .with_plugin_contract(EffectPluginContract::new("0.1.0"))
        .property(PropertyDescriptor::new(
            "plugin.example.soft_blur.radius",
            "Radius",
            PropertyValue::Float(4.0),
        ).with_parameter_id(radius_id.clone()))
        .with_graph(move |effect, context, graph| {
            let radius = effect.evaluate_f32_parameter(&radius_id, context.time, 4.0);
            if radius <= 0.0 {
                return;
            }
            graph.apply(EffectRenderOp::GaussianBlur { radius });
        })
        .build();

    register_effect_definition(definition).expect("register Soft Blur");
}
```

## Example 2: Glow effect (branching graph)

A branching effect that creates a blurred overlay and screen-blends it back.

```rust
use mondrian_core::{
    automation::{PropertyDescriptor, PropertyValue},
    types::BlendMode,
};
use mondrian_effects::{
    EffectPluginContract, EffectPluginDefinitionBuilder, EffectRenderOp, EffectType,
    register_effect_definition,
};

pub fn register() {
    let plugin_type = EffectType::Plugin("plugin.example.glow".to_string());
    let radius_id = plugin_type
        .parameter_id("radius")
        .expect("static parameter ID");
    let opacity_id = plugin_type
        .parameter_id("opacity")
        .expect("static parameter ID");

    let definition = EffectPluginDefinitionBuilder::new(
        plugin_type.key(),
        "Glow",
        EffectColorDomainContract::SCENE_LINEAR,
    )
        .with_execution_contract(current_frame_contract(
            EffectExecutionModes::CPU_F32,
            EffectGraphTopology::GeneralDag,
            EffectResourceLifetime::Frame,
        ))
        .with_plugin_contract(EffectPluginContract::new("0.1.0"))
        .property(PropertyDescriptor::new(
            "plugin.example.glow.radius",
            "Radius",
            PropertyValue::Float(8.0),
        ).with_parameter_id(radius_id.clone()))
        .property(PropertyDescriptor::new(
            "plugin.example.glow.opacity",
            "Opacity",
            PropertyValue::Float(0.4),
        ).with_parameter_id(opacity_id.clone()))
        .with_branching_graph(move |effect, context, graph| {
            let radius = effect.evaluate_f32_parameter(&radius_id, context.time, 8.0);
            let opacity = effect.evaluate_f32_parameter(&opacity_id, context.time, 0.4);

            if radius <= 1.0e-4 || opacity <= 1.0e-4 {
                return;
            }

            graph.blend_current(BlendMode::Screen, opacity, |graph, source| {
                graph.apply_to(source, EffectRenderOp::GaussianBlur { radius })
            });
        })
        .build();

    register_effect_definition(definition).expect("register Glow");
}
```

## Example 3: External LUT boundary

Do not implement an author-selected LUT by carrying its path into
`with_custom_render_backend`, using that path as a deterministic cache key, or
opening the file from a graph/cache-key/processor closure. A path is not content
identity, and all three closures may run on the frame path.

The high-level `EffectPluginDefinitionBuilder` currently has no complete,
versioned “author path → immutable prepared LUT + dependency revalidation”
Interface. Such an instance must therefore return a structured
`EffectGraphBuildError::ResourceUnavailable` and remain blocked. A future
example may be added only when it can bind an immutable LUT payload, exact
content/revision identity, low-frequency revalidation, and the declared
resource lifetime without inventing another cache or filesystem authority.

## Example 4: Mask boundary

`mask(...)` / `mask_current(...)` take `invert`, an explicit
`MaskOp::{Add, Subtract, Intersect, Difference}`, and a closure returning an
`EffectGraphValue`. That value must have `EffectColorDomain::AlphaMask`.

The current high-level plugin DSL does not expose `MaskSource` or a typed
RGB-to-matte producer. Do not fabricate one with raw `EffectRenderOp::Custom`:
it would be missing the Definition-bound processor and an AlphaMask domain
proof, so compilation correctly fails closed. Clip author masks are injected
by the engine; a plugin mask example should be added only when a typed producer
is part of the public SDK.

## Example 5: Registering multiple effects from one plugin

A plugin crate can register multiple effects:

```rust
pub fn register_all()
    -> Result<(), mondrian_effects::effect::EffectDefinitionError>
{
    // Each function builds a complete definition: color domain, exact execution
    // contract, parameter schema, graph builder, and plugin contract.
    register_glow_effect()?;
    register_sharpen_effect()?;
    register_vignette_effect()
}
```

Do not register placeholder definitions that omit their execution contract or
graph builder. The conservative plugin default is intentionally not executable.

## Testing a plugin

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{TimelineTime, WorkingColorSpace};
    use mondrian_effects::{
        build_effect_render_graph, effect_definition, EffectGraphTopology,
        EffectNode, EffectNodeExt, EffectProcessingBackend,
        EffectWorkingPrecision,
    };

    #[test]
    fn glow_effect_produces_valid_graph() {
        register_glow_effect().expect("register Glow");

        let plugin_type = EffectType::Plugin("plugin.example.glow".to_string());
        let effect = EffectNode::with_defaults(plugin_type.clone());

        let graph = build_effect_render_graph(
            &[effect],
            TimelineTime::ZERO,
            WorkingColorSpace::LinearRec709,
        )
        .expect("build Glow graph");
        // Glow with blend creates at least 3 nodes: source → blur → blend
        assert!(graph.nodes.len() >= 3);
        assert!(graph.output.is_some());
        let def = effect_definition(&plugin_type).expect("effect definition should exist");
        let contract = def.execution_contract();
        assert!(contract.execution_modes.contains(
            EffectProcessingBackend::Cpu,
            EffectWorkingPrecision::Float32,
        ));
        assert_eq!(contract.topology, EffectGraphTopology::GeneralDag);
    }
}
```

## Wiring into mondrian-app

In `crates/mondrian-app/Cargo.toml`:
```toml
[dependencies]
mondrian-plugin-example = { path = "../mondrian-plugin-example" }
```

In the app initialization code (typically in `app.rs` or `main.rs`):
```rust
fn init_plugins() {
    mondrian_plugin_example::register();
}
```
