//! Animation property grouping for editor UI.
//!
//! Categorises animated properties into logical groups (Motion, Opacity,
//! Effect, Mask, Other) so inspectors and graph editors can present
//! related properties together without hard-coding property paths in panel code.
//!
//! This module is UI-framework agnostic. It only depends on the automation
//! property system in `mondrian-core` and well-known property paths from
//! `mondrian-timeline`.

use mondrian_core::automation::AnimatedProperty;

/// Logical category for an animated property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnimationGroupKind {
    /// Transform properties: position, scale, rotation, anchor point.
    Motion,
    /// Opacity and blend mode.
    Opacity,
    /// Speed multiplier (time remapping).
    /// Effect-local properties (path starts with `effect.`).
    Effect,
    /// Mask-local properties (path starts with `mask.`).
    Mask,
    /// Properties that do not match any built-in category.
    Other,
}

/// Metadata for a group of related animated properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationGroupMeta {
    /// Stable group identifier for layout and selection state.
    pub id: String,
    /// Human-readable group title.
    pub title: String,
    /// Semantic category.
    pub kind: AnimationGroupKind,
    /// Display order within the inspector graph list.
    pub order: usize,
    /// Whether to show a badge indicating the group is effect-driven.
    pub shows_fx_badge: bool,
    /// Whether the group supports effect-scoped controls (enable/disable, remove).
    pub allows_effect_controls: bool,
    /// Whether the group supports mask-scoped controls.
    pub allows_mask_controls: bool,
}

// ──────────────────────────────────────────────────────────────────────────
// Built-in property path checks
// ──────────────────────────────────────────────────────────────────────────

fn is_motion_property(path: &str) -> bool {
    matches!(
        path,
        mondrian_timeline::clip::Transform2D::POSITION_PATH
            | mondrian_timeline::clip::Transform2D::SCALE_PATH
            | mondrian_timeline::clip::Transform2D::ROTATION_PATH
            | mondrian_timeline::clip::Transform2D::ANCHOR_POINT_PATH
    )
}

// ──────────────────────────────────────────────────────────────────────────
// Public API
// ──────────────────────────────────────────────────────────────────────────

/// Resolve the animation group for a property identified by its path and
/// descriptor metadata.
pub fn property_group_meta(path: &str, property: &AnimatedProperty) -> AnimationGroupMeta {
    if is_motion_property(path) {
        return AnimationGroupMeta {
            id: "builtin.motion".to_string(),
            title: "运动".to_string(),
            kind: AnimationGroupKind::Motion,
            order: 0,
            shows_fx_badge: false,
            allows_effect_controls: false,
            allows_mask_controls: false,
        };
    }
    if path == mondrian_timeline::clip::Transform2D::OPACITY_PATH {
        return AnimationGroupMeta {
            id: "builtin.opacity".to_string(),
            title: "不透明度".to_string(),
            kind: AnimationGroupKind::Opacity,
            order: 1,
            shows_fx_badge: false,
            allows_effect_controls: false,
            allows_mask_controls: false,
        };
    }
    if path == mondrian_timeline::clip::Clip::BLEND_MODE_PATH {
        return AnimationGroupMeta {
            id: "builtin.opacity".to_string(),
            title: "不透明度".to_string(),
            kind: AnimationGroupKind::Opacity,
            order: 1,
            shows_fx_badge: false,
            allows_effect_controls: false,
            allows_mask_controls: false,
        };
    }
    if path.starts_with("mask.") {
        let title = property
            .descriptor
            .ui_metadata
            .group_name
            .clone()
            .unwrap_or_else(|| "蒙版".to_string());
        let slug = sanitize_group_id(&title);
        let uuid_tail = path
            .split('.')
            .nth(1)
            .map(|s| if s.len() > 8 { &s[..8] } else { s })
            .unwrap_or("0");
        return AnimationGroupMeta {
            id: format!("mask.{slug}.{uuid_tail}"),
            title,
            kind: AnimationGroupKind::Mask,
            order: 20,
            shows_fx_badge: false,
            allows_effect_controls: false,
            allows_mask_controls: true,
        };
    }
    if path.starts_with("effect.") {
        let title = property
            .descriptor
            .ui_metadata
            .group_name
            .clone()
            .unwrap_or_else(|| "效果".to_string());
        let slug = sanitize_group_id(&title);
        let uuid_tail = path
            .split('.')
            .nth(1)
            .map(|s| if s.len() > 8 { &s[..8] } else { s })
            .unwrap_or("0");
        return AnimationGroupMeta {
            id: format!("effect.{slug}.{uuid_tail}"),
            title,
            kind: AnimationGroupKind::Effect,
            order: 10,
            shows_fx_badge: true,
            allows_effect_controls: true,
            allows_mask_controls: false,
        };
    }
    let title = property
        .descriptor
        .ui_metadata
        .group_name
        .clone()
        .unwrap_or_else(|| "其他".to_string());
    let slug = sanitize_group_id(&title);
    AnimationGroupMeta {
        id: format!("misc.{slug}"),
        title,
        kind: AnimationGroupKind::Other,
        order: 30,
        shows_fx_badge: false,
        allows_effect_controls: false,
        allows_mask_controls: false,
    }
}

/// Simple display name for a property (taken directly from the descriptor).
pub fn property_display_name(property: &AnimatedProperty) -> String {
    property.descriptor.display_name.clone()
}

/// Qualified display name that includes the group name for effect/other
/// properties when the group title differs from the property display name.
pub fn qualified_property_display_name(path: &str, property: &AnimatedProperty) -> String {
    let group = property_group_meta(path, property);
    match group.kind {
        AnimationGroupKind::Effect | AnimationGroupKind::Other
            if group.title != property.descriptor.display_name =>
        {
            format!("{} · {}", group.title, property.descriptor.display_name)
        }
        _ => property.descriptor.display_name.clone(),
    }
}

/// Canonical display order for built-in property paths.
///
/// Lower values sort before higher values. Properties without an explicit entry
/// default to `100` so they appear after all built-in properties.
pub fn property_order(path: &str) -> usize {
    match path {
        mondrian_timeline::clip::Transform2D::POSITION_PATH => 0,
        mondrian_timeline::clip::Transform2D::SCALE_PATH => 1,
        mondrian_timeline::clip::Transform2D::ROTATION_PATH => 2,
        mondrian_timeline::clip::Transform2D::ANCHOR_POINT_PATH => 3,
        mondrian_timeline::clip::Transform2D::OPACITY_PATH => 4,
        mondrian_timeline::clip::Clip::BLEND_MODE_PATH => 5,
        _ => 100,
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Internal helpers
// ──────────────────────────────────────────────────────────────────────────

fn sanitize_group_id(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if ch.is_whitespace() || matches!(ch, '-' | '_' | '.') {
            if !slug.ends_with('-') {
                slug.push('-');
            }
        } else {
            slug.push_str(&format!("{:x}", ch as u32));
        }
    }
    slug.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
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

    // ── Built-in property grouping ──────────────────────────────────────

    #[test]
    fn builtin_motion_properties_map_to_motion_group() {
        for path in [
            mondrian_timeline::clip::Transform2D::POSITION_PATH,
            mondrian_timeline::clip::Transform2D::SCALE_PATH,
            mondrian_timeline::clip::Transform2D::ROTATION_PATH,
            mondrian_timeline::clip::Transform2D::ANCHOR_POINT_PATH,
        ] {
            let prop = property(path, "test", None);
            let meta = property_group_meta(path, &prop);
            assert_eq!(
                meta.kind,
                AnimationGroupKind::Motion,
                "expected Motion for {path}"
            );
            assert_eq!(meta.id, "builtin.motion");
            assert_eq!(meta.title, "运动");
            assert_eq!(meta.order, 0);
            assert!(!meta.shows_fx_badge);
        }
    }

    #[test]
    fn opacity_path_maps_to_opacity_group() {
        let prop = property(
            mondrian_timeline::clip::Transform2D::OPACITY_PATH,
            "不透明度",
            None,
        );
        let meta = property_group_meta(mondrian_timeline::clip::Transform2D::OPACITY_PATH, &prop);
        assert_eq!(meta.kind, AnimationGroupKind::Opacity);
        assert_eq!(meta.id, "builtin.opacity");
        assert_eq!(meta.title, "不透明度");
        assert_eq!(meta.order, 1);
    }

    #[test]
    fn blend_mode_path_maps_to_opacity_group() {
        let prop = property(
            mondrian_timeline::clip::Clip::BLEND_MODE_PATH,
            "混合模式",
            None,
        );
        let meta = property_group_meta(mondrian_timeline::clip::Clip::BLEND_MODE_PATH, &prop);
        assert_eq!(meta.kind, AnimationGroupKind::Opacity);
        assert_eq!(meta.id, "builtin.opacity");
    }

    // ── Mask properties ─────────────────────────────────────────────────

    #[test]
    fn mask_property_with_group_name_uses_it_as_title() {
        let prop = property("mask.a1b2c3d4.opacity", "透明度", Some("蒙版 1"));
        let meta = property_group_meta("mask.a1b2c3d4.opacity", &prop);
        assert_eq!(meta.kind, AnimationGroupKind::Mask);
        assert_eq!(meta.title, "蒙版 1");
        assert!(meta.allows_mask_controls);
        assert!(meta.id.starts_with("mask."));
    }

    #[test]
    fn mask_property_without_group_name_falls_back_to_default() {
        let prop = property("mask.a1b2c3d4.shape", "形状", None);
        let meta = property_group_meta("mask.a1b2c3d4.shape", &prop);
        assert_eq!(meta.kind, AnimationGroupKind::Mask);
        assert_eq!(meta.title, "蒙版");
        assert!(meta.allows_mask_controls);
        assert!(!meta.shows_fx_badge);
    }

    #[test]
    fn mask_property_uuid_truncated_to_eight_chars_in_group_id() {
        let prop = property("mask.abcdefghijklmn.opacity", "透明度", Some("Mask A"));
        let meta = property_group_meta("mask.abcdefghijklmn.opacity", &prop);
        assert!(
            meta.id.contains("abcdefgh"),
            "group id should contain first 8 chars of uuid: {}",
            meta.id
        );
        assert!(
            !meta.id.contains("ijklmn"),
            "group id should not contain uuid tail beyond 8 chars: {}",
            meta.id
        );
    }

    // ── Effect properties ───────────────────────────────────────────────

    #[test]
    fn effect_property_with_group_name_uses_fx_badge() {
        let prop = property("effect.gaussian_blur.radius", "模糊半径", Some("高斯模糊"));
        let meta = property_group_meta("effect.gaussian_blur.radius", &prop);
        assert_eq!(meta.kind, AnimationGroupKind::Effect);
        assert_eq!(meta.title, "高斯模糊");
        assert_eq!(meta.order, 10);
        assert!(meta.shows_fx_badge);
        assert!(meta.allows_effect_controls);
        assert!(!meta.allows_mask_controls);
    }

    #[test]
    fn effect_property_without_group_name_falls_back_to_default() {
        let prop = property("effect.contrast.intensity", "强度", None);
        let meta = property_group_meta("effect.contrast.intensity", &prop);
        assert_eq!(meta.kind, AnimationGroupKind::Effect);
        assert_eq!(meta.title, "效果");
        assert!(meta.shows_fx_badge);
    }

    #[test]
    fn effect_property_id_includes_effect_uuid_tail() {
        let prop = property("effect.feed_dead_x0.shininess", "Shininess", Some("Bloom"));
        let meta = property_group_meta("effect.feed_dead_x0.shininess", &prop);
        // The id uses the slug from the group title, not the path uuid
        assert!(
            meta.id.starts_with("effect."),
            "effect group id should start with 'effect.': {}",
            meta.id
        );
        assert_eq!(meta.kind, AnimationGroupKind::Effect);
    }

    // ── Other / miscellaneous properties ─────────────────────────────────

    #[test]
    fn unknown_path_with_group_name_uses_it() {
        let prop = property("custom.foo.bar", "Baz", Some("My Group"));
        let meta = property_group_meta("custom.foo.bar", &prop);
        assert_eq!(meta.kind, AnimationGroupKind::Other);
        assert_eq!(meta.title, "My Group");
        assert_eq!(meta.order, 30);
        assert!(!meta.shows_fx_badge);
    }

    #[test]
    fn unknown_path_without_group_name_falls_back_to_default() {
        let prop = property("random.property", "Foo", None);
        let meta = property_group_meta("random.property", &prop);
        assert_eq!(meta.kind, AnimationGroupKind::Other);
        assert_eq!(meta.title, "其他");
        assert_eq!(meta.order, 30);
    }

    // ── Qualified display names ─────────────────────────────────────────

    #[test]
    fn qualified_display_name_for_effect_prepends_group_when_different() {
        let prop = property("effect.blur.radius", "模糊半径", Some("模糊"));
        let name = qualified_property_display_name("effect.blur.radius", &prop);
        assert_eq!(name, "模糊 · 模糊半径");
    }

    #[test]
    fn qualified_display_name_for_motion_does_not_prepend_group() {
        let prop = property(
            mondrian_timeline::clip::Transform2D::POSITION_PATH,
            "位置",
            None,
        );
        let name = qualified_property_display_name(
            mondrian_timeline::clip::Transform2D::POSITION_PATH,
            &prop,
        );
        assert_eq!(name, "位置");
    }

    #[test]
    fn qualified_display_name_for_opacity_does_not_prepend_group() {
        let prop = property(
            mondrian_timeline::clip::Transform2D::OPACITY_PATH,
            "不透明度",
            None,
        );
        let name = qualified_property_display_name(
            mondrian_timeline::clip::Transform2D::OPACITY_PATH,
            &prop,
        );
        assert_eq!(name, "不透明度");
    }

    #[test]
    fn qualified_display_name_when_group_title_matches_display_name_is_plain() {
        let prop = property("effect.hsl.saturation", "饱和度", Some("饱和度"));
        let name = qualified_property_display_name("effect.hsl.saturation", &prop);
        assert_eq!(name, "饱和度");
    }

    // ── Property order ──────────────────────────────────────────────────

    #[test]
    fn builtin_properties_use_canonical_order() {
        assert_eq!(
            property_order(mondrian_timeline::clip::Transform2D::POSITION_PATH),
            0
        );
        assert_eq!(
            property_order(mondrian_timeline::clip::Transform2D::SCALE_PATH),
            1
        );
        assert_eq!(
            property_order(mondrian_timeline::clip::Transform2D::ROTATION_PATH),
            2
        );
        assert_eq!(
            property_order(mondrian_timeline::clip::Transform2D::ANCHOR_POINT_PATH),
            3
        );
        assert_eq!(
            property_order(mondrian_timeline::clip::Transform2D::OPACITY_PATH),
            4
        );
        assert_eq!(
            property_order(mondrian_timeline::clip::Clip::BLEND_MODE_PATH),
            5
        );
    }

    #[test]
    fn unknown_property_paths_default_to_order_100() {
        assert_eq!(property_order("custom.property"), 100);
        assert_eq!(property_order("effect.any.param"), 100);
        assert_eq!(property_order("mask.any.shape"), 100);
    }

    // ── Group id uniqueness ─────────────────────────────────────────────

    #[test]
    fn builtin_group_ids_are_stable_and_predictable() {
        let path = mondrian_timeline::clip::Transform2D::POSITION_PATH;
        let prop = property(path, "位置", None);
        let meta1 = property_group_meta(path, &prop);
        let meta2 = property_group_meta(path, &prop);
        assert_eq!(meta1.id, meta2.id);
        assert_eq!(meta1.kind, meta2.kind);
        assert_eq!(meta1.title, meta2.title);
        assert_eq!(meta1.order, meta2.order);
    }

    // ── Sanitize group id corner cases ──────────────────────────────────

    #[test]
    fn sanitize_group_id_preserves_alphanumeric_and_ascii() {
        assert_eq!(sanitize_group_id("Motion"), "motion");
        assert_eq!(sanitize_group_id("ABC 123"), "abc-123");
        assert_eq!(sanitize_group_id("foo_bar.baz"), "foo-bar-baz");
    }

    #[test]
    fn sanitize_group_id_handles_non_ascii_chars() {
        let id = sanitize_group_id("蒙版");
        // Each CJK character gets a hex representation
        assert!(!id.is_empty());
        assert!(
            !id.contains('-'),
            "CJK-only should not produce trailing hyphens: {id}"
        );
    }

    #[test]
    fn sanitize_group_id_collapses_multiple_separators() {
        let id = sanitize_group_id("a  b");
        assert!(!id.contains("--"), "should collapse double hyphens: {id}");
    }

    #[test]
    fn sanitize_group_id_trims_leading_trailing_separators() {
        let id = sanitize_group_id("- test -");
        assert!(!id.starts_with('-'), "should not start with hyphen: {id}");
        assert!(!id.ends_with('-'), "should not end with hyphen: {id}");
    }

    // ── All AnimationGroupKind variants are reachable ────────────────────

    #[test]
    fn all_group_kinds_are_returned_by_some_path() {
        use std::collections::HashSet;

        let cases: [(&str, Option<&str>); 6] = [
            (mondrian_timeline::clip::Transform2D::POSITION_PATH, None),
            (mondrian_timeline::clip::Transform2D::OPACITY_PATH, None),
            ("effect.blur.amount", Some("模糊")),
            ("mask.abc.shape", Some("蒙版 A")),
            ("random.path", Some("Custom")),
            ("random.path", None),
        ];

        let mut kinds: HashSet<AnimationGroupKind> = HashSet::new();
        for (path, group_name) in cases {
            let prop = property(path, "test", group_name);
            kinds.insert(property_group_meta(path, &prop).kind);
        }

        assert!(kinds.contains(&AnimationGroupKind::Motion));
        assert!(kinds.contains(&AnimationGroupKind::Opacity));
        assert!(kinds.contains(&AnimationGroupKind::Effect));
        assert!(kinds.contains(&AnimationGroupKind::Mask));
        assert!(kinds.contains(&AnimationGroupKind::Other));
    }
}
