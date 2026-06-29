# Effect Spec

An effect instance is `EffectNode`.

## Fields

- `id: EffectId`
- `effect_type: EffectType`
- `properties: PropertyBag`
- `params: serde_json::Value`
- `is_enabled: bool`

## Effect Types

Built-in effect types include basic correction, white balance, LUT, color wheel, curves, HSL, blur/sharpen, vignette, chromatic aberration, grain, chroma key, and luma key.

Plugin effects use `EffectType::Plugin(key)`.

## Property Paths

Definition defaults may use effect-local paths. Once inserted into a clip, properties are namespaced as:

```text
effect.<effect_id>.<effect_namespace>.<parameter>
```

This is not idempotent; code must instantiate an effect once per clip placement.

## Execution Contract

Effects compile to `CompiledEffectGraph`. Each graph node declares operation, inputs, cache policy, and cost. CPU fallback is allowed; GPU execution should be used where supported without changing results.

## Color Behavior

Unless an effect explicitly declares otherwise, effects operate in the sequence working color space after input conversion and before output/display transform.
