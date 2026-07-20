# Complete Examples

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

    let definition = EffectPluginDefinitionBuilder::new(plugin_type.key(), "Soft Blur")
        .with_plugin_contract(EffectPluginContract::new("0.1.0"))
        .property(PropertyDescriptor::new(
            "plugin.example.soft_blur.radius",
            "Radius",
            PropertyValue::Float(4.0),
        ))
        .with_graph(|effect, context, graph| {
            let radius = effect.evaluate_f32_by_suffix(
                "plugin.example.soft_blur.radius", context.time, 4.0
            );
            if radius <= 0.0 {
                return;
            }
            graph.apply(EffectRenderOp::GaussianBlur { radius });
        })
        .build();

    register_effect_definition(definition);
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

    let definition = EffectPluginDefinitionBuilder::new(plugin_type.key(), "Glow")
        .with_plugin_contract(EffectPluginContract::new("0.1.0"))
        .property(PropertyDescriptor::new(
            "plugin.example.glow.radius",
            "Radius",
            PropertyValue::Float(8.0),
        ))
        .property(PropertyDescriptor::new(
            "plugin.example.glow.opacity",
            "Opacity",
            PropertyValue::Float(0.4),
        ))
        .property(PropertyDescriptor::new(
            "plugin.example.glow.threshold",
            "Threshold",
            PropertyValue::Float(0.5),
        ))
        .with_branching_graph(|effect, context, graph| {
            let radius = effect.evaluate_f32_by_suffix(
                "plugin.example.glow.radius", context.time, 8.0
            );
            let opacity = effect.evaluate_f32_by_suffix(
                "plugin.example.glow.opacity", context.time, 0.4
            );

            if radius <= 1.0e-4 || opacity <= 1.0e-4 {
                return;
            }

            graph.blend_current(BlendMode::Screen, opacity, |graph, source| {
                graph.apply_to(source, EffectRenderOp::GaussianBlur { radius });
            });
        })
        .build();

    register_effect_definition(definition);
}
```

## Example 3: LUT loader with custom render backend

A custom render backend that loads a 3D LUT file and applies it.

```rust
use std::sync::Arc;
use mondrian_core::automation::{PropertyDescriptor, PropertyValue};
use mondrian_effects::{
    EffectCachePolicy, EffectPluginContract, EffectPluginDefinitionBuilder, EffectType,
    register_effect_definition,
};

pub fn register() {
    let plugin_type = EffectType::Plugin("plugin.example.lut_loader".to_string());

    let definition = EffectPluginDefinitionBuilder::new(plugin_type.key(), "LUT Loader")
        .with_plugin_contract(
            EffectPluginContract::new("0.1.0")
                .with_runtime_failure_policy(
                    mondrian_effects::EffectPluginRuntimeFailurePolicy::KeepDefinitionAvailable,
                )
                .with_library_policy(
                    mondrian_effects::EffectPluginLibraryPolicy::KeepVisible,
                ),
        )
        .property(PropertyDescriptor::new(
            "plugin.example.lut_loader.path",
            "LUT Path",
            PropertyValue::String(String::new()),
        ))
        .property(PropertyDescriptor::new(
            "plugin.example.lut_loader.intensity",
            "Intensity",
            PropertyValue::Float(1.0),
        ))
        .with_custom_render_backend(
            // Params builder
            Arc::new(|effect, context| {
                let path_id = effect.effect_type.parameter_id("path")
                    .expect("definition parameter ID");
                let path = effect.evaluate_parameter(&path_id, context.time)
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .ok_or_else(|| EffectGraphBuildError::ResourceUnavailable {
                        effect_key: effect.effect_type.key(),
                        effect_id: effect.id,
                        parameter_id: path_id,
                        reason: "LUT path is unbound".to_string(),
                    })?;
                let intensity = effect.evaluate_f32_by_suffix(
                    "plugin.example.lut_loader.intensity", context.time, 1.0
                );
                Ok(Some(serde_json::json!({
                    "lut_path": path,
                    "intensity": intensity,
                })))
            }),
            // Cache key builder
            Some(Arc::new(|effect, context| {
                let path = effect.evaluate_property(
                    "plugin.example.lut_loader.path", context.time
                )?.as_str()?.to_string();
                Some(format!("lut:{}", path))
            })),
            EffectCachePolicy::Deterministic,
            // Processor
            Arc::new(|buffer, width, height, params, frame_seed| {
                let lut_path = params["lut_path"].as_str().unwrap_or("");
                let intensity = params["intensity"].as_f64().unwrap_or(1.0) as f32;

                if lut_path.is_empty() {
                    return Ok(()); // No LUT → identity
                }

                // Apply LUT to buffer pixels...
                // buffer is RGBA, width * height * 4 bytes
                Ok(())
            }),
        )
        .build();

    register_effect_definition(definition);
}
```

## Example 4: Vignette with mask

Using mask_current to create a vignette effect.

```rust
use mondrian_core::automation::{PropertyDescriptor, PropertyValue};
use mondrian_effects::{
    EffectCachePolicy, EffectPluginContract, EffectPluginDefinitionBuilder,
    EffectRenderOp, EffectType, register_effect_definition,
};

pub fn register() {
    let plugin_type = EffectType::Plugin("plugin.example.vignette".to_string());

    let definition = EffectPluginDefinitionBuilder::new(plugin_type.key(), "Vignette")
        .with_plugin_contract(EffectPluginContract::new("0.1.0"))
        .property(PropertyDescriptor::new(
            "plugin.example.vignette.intensity",
            "Intensity",
            PropertyValue::Float(0.5),
        ))
        .with_branching_graph(|effect, context, graph| {
            let intensity = effect.evaluate_f32_by_suffix(
                "plugin.example.vignette.intensity", context.time, 0.5
            );

            if intensity <= 0.0 {
                return;
            }

            // Darken: multiply with a darkened version
            graph.blend_current(
                mondrian_core::types::BlendMode::Multiply,
                intensity,
                |graph, source| {
                    graph.apply_to(source, EffectRenderOp::Vignette {
                        intensity: 1.0,
                        feather: 0.3,
                    });
                },
            );
        })
        .build();

    register_effect_definition(definition);
}
```

## Example 5: Registering multiple effects from one plugin

A plugin crate can register multiple effects:

```rust
pub fn register() {
    register_glow_effect();
    register_sharpen_effect();
    register_vignette_effect();
}

fn register_glow_effect() {
    let plugin_type = EffectType::Plugin("plugin.example.glow".to_string());
    let definition = EffectPluginDefinitionBuilder::new(plugin_type.key(), "Glow")
        .with_plugin_contract(EffectPluginContract::new("0.1.0"))
        // ... build and register
        .build();
    register_effect_definition(definition);
}

fn register_sharpen_effect() {
    let plugin_type = EffectType::Plugin("plugin.example.sharpen".to_string());
    let definition = EffectPluginDefinitionBuilder::new(plugin_type.key(), "Sharpen Pro")
        .with_plugin_contract(EffectPluginContract::new("0.1.0"))
        // ... build and register
        .build();
    register_effect_definition(definition);
}
```

## Testing a plugin

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::types::{Rational, TimeCode};
    use mondrian_effects::{
        build_effect_render_graph, effect_definition, EffectNode,
    };

    #[test]
    fn glow_effect_produces_valid_graph() {
        register();

        let plugin_type = EffectType::Plugin("plugin.example.glow".to_string());
        let effect = EffectNode::new(plugin_type);
        let time = TimeCode::new(0, Rational::new(1, 30));

        let graph = build_effect_render_graph(&[effect], time);
        // Glow with blend creates at least 3 nodes: source → blur → blend
        assert!(graph.nodes.len() >= 3);
        assert!(graph.output.is_some());
    }

    #[test]
    fn glow_effect_is_identity_when_params_zero() {
        register();

        let plugin_type = EffectType::Plugin("plugin.example.glow".to_string());
        let mut effect = EffectNode::new(plugin_type);
        let time = TimeCode::new(0, Rational::new(1, 30));

        // Set both params to zero
        effect.set_static_value_by_suffix(
            "glow.radius",
            mondrian_core::automation::PropertyValue::Float(0.0),
        ).ok();
        effect.set_static_value_by_suffix(
            "glow.opacity",
            mondrian_core::automation::PropertyValue::Float(0.0),
        ).ok();

        let graph = build_effect_render_graph(&[effect], time);
        // With zero params, graph builder returns early → identity graph
        assert!(graph.is_identity());
    }

    #[test]
    fn effect_definition_has_correct_capabilities() {
        register();

        let plugin_type = EffectType::Plugin("plugin.example.glow".to_string());
        let def = effect_definition(&plugin_type).expect("effect definition should exist");
        let caps = def.capabilities();

        assert!(caps.supports_render_graph);
        assert!(caps.supports_branching_render_graph);
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
