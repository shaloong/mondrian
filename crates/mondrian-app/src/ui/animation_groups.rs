use mondrian_core::automation::AnimatedProperty;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationGroupKind {
    Motion,
    Opacity,
    TimeRemap,
    Effect,
    Mask,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationGroupMeta {
    pub id: String,
    pub title: String,
    pub kind: AnimationGroupKind,
    pub order: usize,
    pub shows_fx_badge: bool,
    pub allows_effect_controls: bool,
    pub allows_mask_controls: bool,
}

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
    if path == mondrian_timeline::clip::SpeedMap::MULTIPLIER_PATH {
        return AnimationGroupMeta {
            id: "builtin.time_remap".to_string(),
            title: "时间重映射".to_string(),
            kind: AnimationGroupKind::TimeRemap,
            order: 2,
            shows_fx_badge: false,
            allows_effect_controls: false,
            allows_mask_controls: false,
        };
    }
    if path.starts_with("mask.") {
        // Path format: "mask.<uuid>.<prop>"
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
        // Include UUID from path for uniqueness: "effect.<uuid>.<prop>" → id = "effect.<slug>.<uuid_tail>"
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

pub fn property_display_name(property: &AnimatedProperty) -> String {
    property.descriptor.display_name.clone()
}

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

pub fn property_order(path: &str) -> usize {
    match path {
        mondrian_timeline::clip::Transform2D::POSITION_PATH => 0,
        mondrian_timeline::clip::Transform2D::SCALE_PATH => 1,
        mondrian_timeline::clip::Transform2D::ROTATION_PATH => 2,
        mondrian_timeline::clip::Transform2D::ANCHOR_POINT_PATH => 3,
        mondrian_timeline::clip::Transform2D::OPACITY_PATH => 4,
        mondrian_timeline::clip::Clip::BLEND_MODE_PATH => 5,
        mondrian_timeline::clip::SpeedMap::MULTIPLIER_PATH => 6,
        _ => 100,
    }
}

fn is_motion_property(path: &str) -> bool {
    matches!(
        path,
        mondrian_timeline::clip::Transform2D::POSITION_PATH
            | mondrian_timeline::clip::Transform2D::SCALE_PATH
            | mondrian_timeline::clip::Transform2D::ROTATION_PATH
            | mondrian_timeline::clip::Transform2D::ANCHOR_POINT_PATH
    )
}

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

    #[test]
    fn builtins_map_to_expected_groups() {
        let motion = property(
            mondrian_timeline::clip::Transform2D::POSITION_PATH,
            "位置",
            None,
        );
        let opacity = property(
            mondrian_timeline::clip::Transform2D::OPACITY_PATH,
            "不透明度",
            None,
        );
        let speed = property(
            mondrian_timeline::clip::SpeedMap::MULTIPLIER_PATH,
            "速度倍数",
            None,
        );
        let blend = property(
            mondrian_timeline::clip::Clip::BLEND_MODE_PATH,
            "混合模式",
            None,
        );

        assert_eq!(
            property_group_meta(mondrian_timeline::clip::Transform2D::POSITION_PATH, &motion).kind,
            AnimationGroupKind::Motion
        );
        assert_eq!(
            property_group_meta(mondrian_timeline::clip::Transform2D::OPACITY_PATH, &opacity).kind,
            AnimationGroupKind::Opacity
        );
        assert_eq!(
            property_group_meta(mondrian_timeline::clip::SpeedMap::MULTIPLIER_PATH, &speed).kind,
            AnimationGroupKind::TimeRemap
        );
        assert_eq!(
            property_group_meta(mondrian_timeline::clip::Clip::BLEND_MODE_PATH, &blend).kind,
            AnimationGroupKind::Opacity
        );
    }

    #[test]
    fn effect_properties_use_fx_groups() {
        let blur = property("effect.gaussian_blur.radius", "模糊半径", Some("模糊"));
        let meta = property_group_meta("effect.gaussian_blur.radius", &blur);
        assert_eq!(meta.kind, AnimationGroupKind::Effect);
        assert!(meta.shows_fx_badge);
        assert!(meta.allows_effect_controls);
        assert_eq!(
            qualified_property_display_name("effect.gaussian_blur.radius", &blur),
            "模糊 · 模糊半径"
        );
    }
}
