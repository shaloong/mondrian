//! Sequence-owned lights and their immutable per-frame render values.

use mondrian_core::{
    AuthoringFootprint, AuthoringFootprintCollector, AuthoringFootprintError, ExactAutomationCurve,
    KeyframeId, ParameterId, SceneLightId, TimelineTime, TimelineTimeError, TimelineTimeRange,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use mondrian_core::{SceneLightFalloff, SceneLightSample, SceneLightSource};

/// Numeric channels supported by the light's selected geometry and falloff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneLightParameter {
    Red,
    Green,
    Blue,
    Intensity,
    PositionX,
    PositionY,
    PositionZ,
    TargetX,
    TargetY,
    TargetZ,
    ConeAngle,
    ConeFeather,
    FalloffRadius,
    FalloffDistance,
}

impl SceneLightParameter {
    const ALL: [Self; 14] = [
        Self::Red,
        Self::Green,
        Self::Blue,
        Self::Intensity,
        Self::PositionX,
        Self::PositionY,
        Self::PositionZ,
        Self::TargetX,
        Self::TargetY,
        Self::TargetZ,
        Self::ConeAngle,
        Self::ConeFeather,
        Self::FalloffRadius,
        Self::FalloffDistance,
    ];

    /// Stable schema identity used by the shared exact automation curve.
    pub fn parameter_id(self) -> ParameterId {
        let path = match self {
            Self::Red => "mondrian.scene_light.red",
            Self::Green => "mondrian.scene_light.green",
            Self::Blue => "mondrian.scene_light.blue",
            Self::Intensity => "mondrian.scene_light.intensity",
            Self::PositionX => "mondrian.scene_light.position_x",
            Self::PositionY => "mondrian.scene_light.position_y",
            Self::PositionZ => "mondrian.scene_light.position_z",
            Self::TargetX => "mondrian.scene_light.target_x",
            Self::TargetY => "mondrian.scene_light.target_y",
            Self::TargetZ => "mondrian.scene_light.target_z",
            Self::ConeAngle => "mondrian.scene_light.cone_angle",
            Self::ConeFeather => "mondrian.scene_light.cone_feather",
            Self::FalloffRadius => "mondrian.scene_light.falloff_radius",
            Self::FalloffDistance => "mondrian.scene_light.falloff_distance",
        };
        ParameterId::new_static(path)
    }

    /// Inclusive range of an authored static value or keyframe.
    pub fn hard_range(self) -> std::ops::RangeInclusive<f64> {
        let limit = f64::from(f32::MAX);
        match self {
            Self::Red | Self::Green | Self::Blue | Self::FalloffRadius | Self::FalloffDistance => {
                0.0..=limit
            }
            Self::ConeAngle => 0.0..=180.0,
            Self::ConeFeather => 0.0..=100.0,
            Self::Intensity
            | Self::PositionX
            | Self::PositionY
            | Self::PositionZ
            | Self::TargetX
            | Self::TargetY
            | Self::TargetZ => -limit..=limit,
        }
    }

    fn checked_value(self, value: f64) -> Result<f32, SceneLightError> {
        if !self.hard_range().contains(&value) {
            return Err(SceneLightError::ValueOutOfRange { parameter: self, value });
        }
        // Canonicalize signed zero so equivalent values have one render identity.
        Ok(if value == 0.0 { 0.0 } else { value as f32 })
    }
}

fn parameter_mut(
    sample: &mut SceneLightSample,
    parameter: SceneLightParameter,
) -> Option<&mut f32> {
    use SceneLightParameter::*;
    use SceneLightSource::*;
    match parameter {
        Red => Some(&mut sample.color[0]),
        Green => Some(&mut sample.color[1]),
        Blue => Some(&mut sample.color[2]),
        Intensity => Some(&mut sample.intensity_percent),
        PositionX | PositionY | PositionZ => match &mut sample.source {
            Parallel { position, .. } | Point { position, .. } | Spot { position, .. } => {
                let axis = parameter as usize - PositionX as usize;
                Some(&mut position[axis])
            }
            Ambient => None,
        },
        TargetX | TargetY | TargetZ => match &mut sample.source {
            Parallel { target, .. } | Spot { target, .. } => {
                let axis = parameter as usize - TargetX as usize;
                Some(&mut target[axis])
            }
            Ambient | Point { .. } => None,
        },
        ConeAngle => match &mut sample.source {
            Spot { cone_angle_degrees, .. } => Some(cone_angle_degrees),
            Ambient | Parallel { .. } | Point { .. } => None,
        },
        ConeFeather => match &mut sample.source {
            Spot { cone_feather_percent, .. } => Some(cone_feather_percent),
            Ambient | Parallel { .. } | Point { .. } => None,
        },
        FalloffRadius | FalloffDistance => match &mut sample.source {
            Parallel { falloff, .. } | Point { falloff, .. } | Spot { falloff, .. } => {
                match (parameter, falloff) {
                    (FalloffRadius, SceneLightFalloff::Smooth { radius, .. })
                    | (FalloffRadius, SceneLightFalloff::InverseSquareClamped { radius }) => {
                        Some(radius)
                    }
                    (FalloffDistance, SceneLightFalloff::Smooth { distance, .. }) => Some(distance),
                    _ => None,
                }
            }
            Ambient => None,
        },
    }
}

fn validate_direction(sample: SceneLightSample) -> Result<(), SceneLightError> {
    match sample.source {
        SceneLightSource::Parallel { position, target, .. }
        | SceneLightSource::Spot { position, target, .. }
            if position == target =>
        {
            Err(SceneLightError::CoincidentPositionAndTarget)
        }
        _ => Ok(()),
    }
}

/// Persistent author state for one light. An absent range means the light is
/// enabled throughout the Sequence. Automation keys use absolute Sequence time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneLight {
    pub id: SceneLightId,
    pub name: String,
    pub enabled: bool,
    pub active_range: Option<TimelineTimeRange>,
    pub values: SceneLightSample,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub automation: BTreeMap<SceneLightParameter, ExactAutomationCurve>,
}

impl SceneLight {
    pub fn new(name: impl Into<String>, values: SceneLightSample) -> Self {
        Self {
            id: SceneLightId::new(),
            name: name.into(),
            enabled: true,
            active_range: None,
            values,
            automation: BTreeMap::new(),
        }
    }

    /// Read a supported channel's static author value.
    pub fn static_parameter(&self, parameter: SceneLightParameter) -> Option<f32> {
        let mut values = self.values;
        parameter_mut(&mut values, parameter).copied()
    }

    /// Change one static channel, leaving multi-channel geometry validation to
    /// the owner of the complete draft.
    pub fn set_static_parameter(
        &mut self,
        parameter: SceneLightParameter,
        value: f64,
    ) -> Result<(), SceneLightError> {
        let value = parameter.checked_value(value)?;
        *parameter_mut(&mut self.values, parameter)
            .ok_or(SceneLightError::InactiveParameter(parameter))? = value;
        Ok(())
    }

    /// Validate author data at edit and project admission.
    pub fn validate(&self) -> Result<(), SceneLightError> {
        if self.name.trim().is_empty() {
            return Err(SceneLightError::EmptyName);
        }
        if let Some(range) = self.active_range {
            if range.duration <= TimelineTime::ZERO {
                return Err(SceneLightError::EmptyActiveRange);
            }
            range.end()?;
        }
        let mut values = self.values;
        for parameter in SceneLightParameter::ALL {
            if let Some(value) = parameter_mut(&mut values, parameter) {
                parameter.checked_value(f64::from(*value))?;
            }
        }
        validate_direction(self.values)?;
        for (&parameter, curve) in &self.automation {
            if parameter_mut(&mut values, parameter).is_none() {
                return Err(SceneLightError::InactiveParameter(parameter));
            }
            if curve.parameter_id != parameter.parameter_id() {
                return Err(SceneLightError::WrongCurveIdentity(parameter));
            }
            curve.validate()?;
            parameter.checked_value(curve.default_value)?;
            for key in &curve.keyframes {
                parameter.checked_value(key.value)?;
            }
        }
        Ok(())
    }

    /// Evaluate the active light without using a render cursor or frame history.
    /// The returned values contain no author names or object identities.
    pub fn sample(&self, time: TimelineTime) -> Result<Option<SceneLightSample>, SceneLightError> {
        if !self.enabled || self.active_range.is_some_and(|range| time < range.start) {
            return Ok(None);
        }
        if let Some(range) = self.active_range
            && time >= range.end()?
        {
            return Ok(None);
        }
        let mut values = self.values;
        for parameter in SceneLightParameter::ALL {
            if let Some(value) = parameter_mut(&mut values, parameter) {
                let sampled = self
                    .automation
                    .get(&parameter)
                    .map(|curve| curve.evaluate(time))
                    .transpose()?
                    .unwrap_or_else(|| f64::from(*value));
                *value = parameter.checked_value(sampled)?;
            }
        }
        validate_direction(values)?;
        Ok(Some(values))
    }

    pub(crate) fn fork_author_identities(&mut self) {
        self.id = SceneLightId::new();
        for curve in self.automation.values_mut() {
            for key in &mut curve.keyframes {
                key.id = KeyframeId::new();
            }
        }
    }
}

impl AuthoringFootprint for SceneLight {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        let Self {
            id: _,
            name,
            enabled: _,
            active_range: _,
            values: _,
            automation,
        } = self;
        collector.collect(name)?;
        collector.collect(automation)
    }
}

// The channel key owns no heap allocation.
impl AuthoringFootprint for SceneLightParameter {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SceneLightError {
    #[error("scene light name cannot be empty")]
    EmptyName,
    #[error("scene light active range must have positive duration")]
    EmptyActiveRange,
    #[error("a directed light's position and target must differ")]
    CoincidentPositionAndTarget,
    #[error("scene light parameter {0:?} is unavailable for this light")]
    InactiveParameter(SceneLightParameter),
    #[error("scene light parameter {0:?} has a mismatched curve identity")]
    WrongCurveIdentity(SceneLightParameter),
    #[error("scene light parameter {parameter:?} has an out-of-range value {value}")]
    ValueOutOfRange {
        parameter: SceneLightParameter,
        value: f64,
    },
    #[error(transparent)]
    Time(#[from] TimelineTimeError),
    #[error(transparent)]
    Automation(#[from] mondrian_core::AutomationError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Sequence;
    use mondrian_core::{ExactAutomationKeyframe, ProjectColorEnvironment};

    #[test]
    fn scene_light_automation_uses_exact_time_and_half_open_activity() {
        let start = TimelineTime::new(1001, 30000).unwrap();
        let middle = TimelineTime::new(1001, 20000).unwrap();
        let end = TimelineTime::new(1001, 15000).unwrap();
        let mut light = SceneLight::new("Key light", SceneLightSample::default());
        light.active_range = Some(TimelineTimeRange::new(start, start).unwrap());
        let intensity = SceneLightParameter::Intensity;
        let mut curve = ExactAutomationCurve::new(intensity.parameter_id(), 100.0).unwrap();
        curve.set_keyframe(ExactAutomationKeyframe::linear(start, -50.0)).unwrap();
        curve.set_keyframe(ExactAutomationKeyframe::linear(end, 150.0)).unwrap();
        light.automation.insert(intensity, curve);
        light.validate().unwrap();
        assert_eq!(light.sample(end).unwrap(), None);
        assert_eq!(
            light.sample(middle).unwrap().unwrap().intensity_percent,
            50.0
        );
        assert_eq!(
            light.sample(start).unwrap().unwrap().intensity_percent,
            -50.0
        );
        assert_eq!(light.sample(TimelineTime::ZERO).unwrap(), None);
        light.enabled = false;
        assert_eq!(light.sample(middle).unwrap(), None);
    }

    #[test]
    fn scene_lights_round_trip_and_fork_sequence_owned_identities() {
        let mut sequence = Sequence::new("Lighting");
        let legacy = serde_json::to_value(&sequence).unwrap();
        assert!(legacy.get("scene_lights").is_none());
        let loaded: Sequence = serde_json::from_value(legacy).unwrap();
        assert!(loaded.scene_lights.is_empty());
        let mut light = SceneLight::new(
            "Spot",
            SceneLightSample {
                source: SceneLightSource::Spot {
                    position: [8.0, 6.0, 10.0],
                    target: [10.0, 8.0, 0.0],
                    cone_angle_degrees: 75.0,
                    cone_feather_percent: 45.0,
                    falloff: SceneLightFalloff::Smooth { radius: 4.0, distance: 32.0 },
                },
                ..SceneLightSample::default()
            },
        );
        let parameter = SceneLightParameter::ConeAngle;
        let mut curve = ExactAutomationCurve::new(parameter.parameter_id(), 75.0).unwrap();
        curve
            .set_keyframe(ExactAutomationKeyframe::linear(TimelineTime::ONE, 80.0))
            .unwrap();
        light.automation.insert(parameter, curve);
        sequence.scene_lights.push(light.clone());
        sequence.validate_author_contract(&ProjectColorEnvironment::default()).unwrap();
        assert!(!sequence.author_state_eq_ignoring_revision(&loaded));
        let serialized = serde_json::to_vec(&sequence).unwrap();
        let mut duplicate: Sequence = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(sequence, duplicate);
        duplicate.fork_author_identities_for_sequence_duplicate();
        let duplicate = &duplicate.scene_lights[0];
        assert_ne!(duplicate.id, light.id);
        assert_ne!(
            duplicate.automation[&parameter].keyframes[0].id,
            light.automation[&parameter].keyframes[0].id
        );
        assert_eq!(
            duplicate.sample(TimelineTime::ONE).unwrap(),
            light.sample(TimelineTime::ONE).unwrap()
        );
    }

    #[test]
    fn scene_light_validation_checks_ids_curve_channels_and_direction() {
        let mut light = SceneLight::new("Ambient", SceneLightSample::default());
        let key = SceneLightParameter::ConeFeather;
        let curve = ExactAutomationCurve::new(key.parameter_id(), 30.0).unwrap();
        light.automation.insert(key, curve);
        assert_eq!(
            light.validate(),
            Err(SceneLightError::InactiveParameter(key))
        );
        light.automation.clear();
        light.values.source = SceneLightSource::Parallel {
            position: [0.0; 3],
            target: [0.0; 3],
            falloff: SceneLightFalloff::None,
        };
        assert_eq!(
            light.validate(),
            Err(SceneLightError::CoincidentPositionAndTarget)
        );
        light.values.source = SceneLightSource::Ambient;
        let mut sequence = Sequence::new("Invalid lights");
        sequence.scene_lights = [light.clone(), light].into();
        let error = sequence.validate_author_identities().unwrap_err();
        assert!(error.to_string().contains("duplicate scene light identity"));
    }
}
