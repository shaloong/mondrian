//! App-owned author curve fixture preparation.

/// Lower a fixture UI seed to an exact request before exercising author admission.
pub(crate) fn curve_author_point(
    state: &super::AppState,
    clip_id: mondrian_core::ClipId,
    address: &mondrian_core::automation::AnimationParameterAddress,
    time_ratio: f64,
    value_ratio: f64,
) -> super::product_action::ClipCurveAuthorPoint {
    let clip = state
        .active_sequence()
        .expect("fixture sequence")
        .find_clip(clip_id)
        .expect("fixture clip");
    let bag = clip.intrinsic_parameter_bag();
    let property = bag
        .property_by_address(address)
        .map(|(_, property)| property)
        .or_else(|| {
            clip.effects.iter().find_map(|effect| {
                effect.properties.property_by_address(address).map(|(_, property)| property)
            })
        })
        .expect("fixture parameter");
    let range = mondrian_core::TimelineTimeRange::new(clip.clip_time_in, clip.duration)
        .expect("fixture author span");
    super::product_action::ClipCurveAuthorPoint {
        time: super::numeric_curve_author_time(range, time_ratio).expect("fixture exact time"),
        value: super::numeric_curve_author_value(
            property,
            property.descriptor.schema.numeric.as_ref().expect("fixture numeric")[0]
                .soft_range
                .value_at_normalized_position(value_ratio)
                .expect("fixture UI seed"),
        )
        .expect("fixture typed value"),
    }
}
