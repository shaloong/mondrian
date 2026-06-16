//! Re-exports from `mondrian-editor-state::animation_groups`.
//!
//! This module is a thin bridge: egui code continues to reference
//! `crate::egui_ui::animation_groups` while the real implementation lives in the
//! UI-framework-agnostic editor-state crate.
//
// TODO: delete this file once egui panels are fully replaced.

pub use mondrian_editor_state::animation_groups::{
    property_display_name, property_group_meta, property_order, qualified_property_display_name,
    AnimationGroupKind, AnimationGroupMeta,
};

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::automation::AnimatedProperty;
    use mondrian_core::automation::{
        AnimatablePropertyUiMetadata, PropertyDescriptor, PropertyValue,
    };

    fn property(path: &str, display_name: &str, group_name: Option<&str>) -> AnimatedProperty {
        let mut descriptor = PropertyDescriptor::new(path, display_name, PropertyValue::Float(0.0));
        descriptor.ui_metadata = AnimatablePropertyUiMetadata {
            group_name: group_name.map(str::to_string),
            ..Default::default()
        };
        AnimatedProperty::from_descriptor(descriptor)
    }

    #[test]
    fn re_exports_work_for_builtins() {
        let prop = property(
            mondrian_timeline::clip::Transform2D::POSITION_PATH,
            "位置",
            None,
        );
        let meta = property_group_meta(mondrian_timeline::clip::Transform2D::POSITION_PATH, &prop);
        assert_eq!(meta.kind, AnimationGroupKind::Motion);
    }

    #[test]
    fn re_exports_work_for_effect_properties() {
        let prop = property("effect.blur.radius", "模糊半径", Some("高斯模糊"));
        let name = qualified_property_display_name("effect.blur.radius", &prop);
        assert_eq!(name, "高斯模糊 · 模糊半径");
    }
}
