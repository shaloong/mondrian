//! Static authoring controls for Sequence lights.

use super::{sequence_text, AppUiSequenceSettingsDraft, Localizer};
use crate::app::ui_actions::{
    app_shell_sequence_settings_draft_changed_action, SceneLightDraftEdit, SceneLightFalloffMode,
    SequenceSettingsDraftUpdatePayload,
};
use mondrian_core::{SceneLightFalloff, SceneLightSample, SceneLightSource};
use mondrian_timeline::scene_light::{SceneLight, SceneLightParameter};
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::menu::{Dropdown, MenuItem};
use mondrian_ui_widgets::{Button, Checkbox, Label, NumberInput, TextInput};

impl AppUiSequenceSettingsDraft {
    fn current_light(&self) -> Option<&SceneLight> {
        self.scene_lights.iter().find(|light| Some(light.id) == self.selected_light)
    }

    pub(super) fn edit_scene_light(&mut self, edit: SceneLightDraftEdit) {
        let id = match edit {
            SceneLightDraftEdit::Select(id) => {
                if self.scene_lights.iter().any(|light| light.id == id) {
                    self.selected_light = Some(id);
                }
                return;
            }
            SceneLightDraftEdit::Add { name, source } => {
                let light =
                    SceneLight::new(name, SceneLightSample { source, ..Default::default() });
                self.selected_light = Some(light.id);
                self.scene_lights.push(light);
                self.scene_lights_changed = true;
                return;
            }
            SceneLightDraftEdit::Remove(id)
            | SceneLightDraftEdit::Name(id, _)
            | SceneLightDraftEdit::Enabled(id, _)
            | SceneLightDraftEdit::Parameter(id, ..)
            | SceneLightDraftEdit::Falloff(id, _) => id,
        };
        let Some(index) = self.scene_lights.iter().position(|light| light.id == id) else {
            return;
        };
        if matches!(edit, SceneLightDraftEdit::Remove(_)) {
            self.scene_lights.remove(index);
            if self.selected_light == Some(id) {
                self.selected_light =
                    self.scene_lights.get(index.saturating_sub(1)).map(|light| light.id);
            }
            self.scene_lights_changed = true;
            return;
        }
        let light = &mut self.scene_lights[index];
        match edit {
            SceneLightDraftEdit::Name(_, name) => light.name = name,
            SceneLightDraftEdit::Enabled(_, enabled) => light.enabled = enabled,
            SceneLightDraftEdit::Parameter(_, parameter, value) => {
                if light.automation.contains_key(&parameter)
                    || light.set_static_parameter(parameter, value).is_err()
                {
                    return;
                }
            }
            SceneLightDraftEdit::Falloff(_, mode) => {
                if has_animated_falloff(light) {
                    return;
                }
                let radius =
                    light.static_parameter(SceneLightParameter::FalloffRadius).unwrap_or(100.0);
                let distance =
                    light.static_parameter(SceneLightParameter::FalloffDistance).unwrap_or(500.0);
                let value = match mode {
                    SceneLightFalloffMode::None => SceneLightFalloff::None,
                    SceneLightFalloffMode::Smooth => SceneLightFalloff::Smooth { radius, distance },
                    SceneLightFalloffMode::InverseSquare => {
                        SceneLightFalloff::InverseSquareClamped { radius }
                    }
                };
                match &mut light.values.source {
                    SceneLightSource::Ambient => return,
                    SceneLightSource::Parallel { falloff, .. }
                    | SceneLightSource::Point { falloff, .. }
                    | SceneLightSource::Spot { falloff, .. } => *falloff = value,
                }
            }
            SceneLightDraftEdit::Add { .. }
            | SceneLightDraftEdit::Select(_)
            | SceneLightDraftEdit::Remove(_) => unreachable!("handled above"),
        }
        self.scene_lights_changed = true;
    }
}

fn action(edit: SceneLightDraftEdit) -> mondrian_editor_state::Action {
    app_shell_sequence_settings_draft_changed_action(
        SequenceSettingsDraftUpdatePayload::SceneLight(edit),
    )
}

fn has_animated_falloff(light: &SceneLight) -> bool {
    use SceneLightParameter::{FalloffDistance, FalloffRadius};
    [FalloffRadius, FalloffDistance]
        .iter()
        .any(|parameter| light.automation.contains_key(parameter))
}

fn light_kind(source: SceneLightSource) -> (&'static str, &'static str) {
    match source {
        SceneLightSource::Ambient => ("scene-light-ambient", "环境光"),
        SceneLightSource::Parallel { .. } => ("scene-light-parallel", "平行光"),
        SceneLightSource::Point { .. } => ("scene-light-point", "点光源"),
        SceneLightSource::Spot { .. } => ("scene-light-spot", "聚光灯"),
    }
}

fn selection_model(
    draft: &AppUiSequenceSettingsDraft,
    localizer: Option<&Localizer>,
) -> (String, Vec<MenuItem>) {
    let title = |light: &SceneLight| {
        let (key, fallback) = light_kind(light.values.source);
        format!(
            "{} · {}",
            light.name,
            sequence_text(localizer, key, fallback)
        )
    };
    let label = draft.current_light().map_or_else(
        || sequence_text(localizer, "scene-light-empty", "没有合成灯"),
        title,
    );
    let items = draft
        .scene_lights
        .iter()
        .map(|light| MenuItem::new(title(light), action(SceneLightDraftEdit::Select(light.id))))
        .collect();
    (label, items)
}

struct Field {
    label: Label,
    input: NumberInput<f64>,
    column: u8,
    row: u8,
}

impl Field {
    fn new(
        light: &SceneLight,
        parameter: SceneLightParameter,
        caption: String,
        column: u8,
        row: u8,
    ) -> Option<Self> {
        use SceneLightParameter::{Blue, Green, Red};
        let (min, max) = parameter.hard_range().into_inner();
        let value = light.static_parameter(parameter)?;
        let id = light.id;
        let mut input = NumberInput::new(f64::from(value), min, max)
            .with_decimals(3)
            .with_step(if matches!(parameter, Red | Green | Blue) {
                0.01
            } else {
                1.0
            })
            .enabled(!light.automation.contains_key(&parameter))
            .on_change(move |value| action(SceneLightDraftEdit::Parameter(id, parameter, value)));
        input.set_accessible_name(Some(caption.clone()));
        Some(Self {
            label: Label::new(caption).muted().with_padding(0.0, 0.0),
            input,
            column,
            row,
        })
    }
}

pub(super) struct SceneLightEditor {
    id: WidgetId,
    bounds: Rect,
    select: Dropdown,
    add: Dropdown,
    remove: Button,
    light: Option<LightControls>,
}

struct LightControls {
    name: TextInput,
    enabled: Checkbox,
    falloff: Option<Dropdown>,
    fields: Vec<Field>,
    animated: Option<Label>,
}

impl SceneLightEditor {
    pub(super) fn new(draft: &AppUiSequenceSettingsDraft, localizer: Option<&Localizer>) -> Self {
        let text = |id, fallback| sequence_text(localizer, id, fallback);
        let (label, choices) = selection_model(draft, localizer);
        let select = Dropdown::new(label, choices)
            .with_max_visible_items(6)
            .enabled(!draft.scene_lights.is_empty());
        let aspect = draft.settings.pixel_aspect_ratio.ratio().unwrap_or(1.0);
        let x = draft.settings.resolution.width as f32 * aspect * 0.5;
        let y = draft.settings.resolution.height as f32 * 0.5;
        let position = [x, y, 300.0];
        let target = [x, y, 0.0];
        let falloff = SceneLightFalloff::None;
        let add = Dropdown::new(
            text("scene-light-add", "添加灯光"),
            [
                SceneLightSource::Ambient,
                SceneLightSource::Parallel { position, target, falloff },
                SceneLightSource::Point { position, falloff },
                SceneLightSource::Spot {
                    position,
                    target,
                    falloff,
                    cone_angle_degrees: 90.0,
                    cone_feather_percent: 25.0,
                },
            ]
            .into_iter()
            .map(|source| {
                let (id, fallback) = light_kind(source);
                let name = text(id, fallback);
                MenuItem::new(
                    name.clone(),
                    action(SceneLightDraftEdit::Add { name, source }),
                )
            })
            .collect(),
        );
        let current = draft.current_light();
        let remove = Button::new(text("scene-light-remove", "移除灯光"))
            .enabled(current.is_some())
            .on_click(current.map(|light| action(SceneLightDraftEdit::Remove(light.id))));
        Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            select,
            add,
            remove,
            light: current.map(|light| LightControls::new(light, localizer)),
        }
    }

    pub(super) fn update_selected_name(
        &mut self,
        draft: &AppUiSequenceSettingsDraft,
        localizer: Option<&Localizer>,
    ) {
        let (label, choices) = selection_model(draft, localizer);
        self.select.set_model(label, choices);
    }
}

impl LightControls {
    fn new(light: &SceneLight, localizer: Option<&Localizer>) -> Self {
        use SceneLightParameter::*;
        let text = |id, fallback| sequence_text(localizer, id, fallback);
        let id = light.id;
        let name = TextInput::new(text("scene-light-name", "灯光名称"))
            .with_text(&light.name)
            .on_change(move |name| action(SceneLightDraftEdit::Name(id, name.to_owned())));
        let enabled = Checkbox::new(text("scene-light-enabled", "启用灯光"), light.enabled)
            .on_change(move |enabled| action(SceneLightDraftEdit::Enabled(id, enabled)));
        let current_falloff = match light.values.source {
            SceneLightSource::Ambient => None,
            SceneLightSource::Parallel { falloff, .. }
            | SceneLightSource::Point { falloff, .. }
            | SceneLightSource::Spot { falloff, .. } => Some(falloff),
        };
        let falloff = current_falloff.map(|falloff| {
            let labels = [
                text("scene-light-falloff-none", "衰减：无"),
                text("scene-light-falloff-smooth", "衰减：平滑"),
                text("scene-light-falloff-inverse-square", "衰减：平方反比"),
            ];
            let selected = match falloff {
                SceneLightFalloff::None => 0,
                SceneLightFalloff::Smooth { .. } => 1,
                SceneLightFalloff::InverseSquareClamped { .. } => 2,
            };
            Dropdown::new(
                labels[selected].clone(),
                labels
                    .into_iter()
                    .zip([
                        SceneLightFalloffMode::None,
                        SceneLightFalloffMode::Smooth,
                        SceneLightFalloffMode::InverseSquare,
                    ])
                    .map(|(label, value)| {
                        MenuItem::new(label, action(SceneLightDraftEdit::Falloff(id, value)))
                    })
                    .collect(),
            )
            .enabled(!has_animated_falloff(light))
        });
        let fields = [
            (Red, "scene-light-red", "红色", 0, 0),
            (Green, "scene-light-green", "绿色", 1, 0),
            (Blue, "scene-light-blue", "蓝色", 2, 0),
            (Intensity, "scene-light-intensity", "强度 (%)", 3, 0),
            (PositionX, "scene-light-position-x", "位置 X", 0, 1),
            (PositionY, "scene-light-position-y", "位置 Y", 1, 1),
            (PositionZ, "scene-light-position-z", "位置 Z", 2, 1),
            (TargetX, "scene-light-target-x", "目标 X", 0, 2),
            (TargetY, "scene-light-target-y", "目标 Y", 1, 2),
            (TargetZ, "scene-light-target-z", "目标 Z", 2, 2),
            (ConeAngle, "scene-light-cone-angle", "锥角 (°)", 0, 3),
            (
                ConeFeather,
                "scene-light-cone-feather",
                "锥边羽化 (%)",
                1,
                3,
            ),
            (
                FalloffRadius,
                "scene-light-falloff-radius",
                "衰减半径",
                2,
                3,
            ),
            (
                FalloffDistance,
                "scene-light-falloff-distance",
                "衰减距离",
                3,
                3,
            ),
        ]
        .into_iter()
        .filter_map(|(parameter, id, fallback, column, row)| {
            Field::new(light, parameter, text(id, fallback), column, row)
        })
        .collect();
        let animated = (!light.automation.is_empty()).then(|| {
            Label::new(text(
                "scene-light-animated",
                "已有动画的字段保留关键帧，无法在此编辑。",
            ))
            .muted()
            .with_padding(0.0, 0.0)
        });
        Self { name, enabled, falloff, fields, animated }
    }
}

impl Widget for SceneLightEditor {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(640.0, 290.0))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let width = ((bounds.width - 30.0) * 0.25).max(0.0);
        let pitch = width + 10.0;
        let x = |column: u8| bounds.x + pitch * f32::from(column);
        self.select.layout(Rect::new(x(0), bounds.y, width * 2.0 + 10.0, 30.0));
        self.add.layout(Rect::new(x(2), bounds.y, width, 30.0));
        self.remove.layout(Rect::new(x(3), bounds.y, width, 30.0));
        if let Some(light) = &mut self.light {
            let details = bounds.y + 44.0;
            light.name.layout(Rect::new(x(0), details, width * 2.0 + 10.0, 28.0));
            light.enabled.layout(Rect::new(x(2), details, width, 28.0));
            if let Some(falloff) = &mut light.falloff {
                falloff.layout(Rect::new(x(3), details, width, 28.0));
            }
            for field in &mut light.fields {
                let y = details + 36.0 + f32::from(field.row) * 43.0;
                let x = x(field.column);
                field.label.layout(Rect::new(x, y, width, 15.0));
                field.input.layout(Rect::new(x, y + 16.0, width, 26.0));
            }
            if let Some(animated) = &mut light.animated {
                animated.layout(Rect::new(x(0), details + 212.0, bounds.width, 19.0));
            }
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        for index in 0..self.child_count() {
            if let Some(widget) = self.child_mut(index)
                && widget.event(event, ctx) == EventResult::Handled
            {
                return EventResult::Handled;
            }
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        if let Some(light) = &self.light {
            for field in &light.fields {
                field.label.paint(ctx);
                field.input.paint(ctx);
            }
            if let Some(animated) = &light.animated {
                animated.paint(ctx);
            }
            light.name.paint(ctx);
            light.enabled.paint(ctx);
            if let Some(falloff) = &light.falloff {
                falloff.paint(ctx);
            }
        }
        self.remove.paint(ctx);
        self.add.paint(ctx);
        self.select.paint(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        self.light.as_ref().map_or(3, |light| 6 + light.fields.len())
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.select),
            1 => Some(&self.add),
            2 => Some(&self.remove),
            3 => Some(self.light.as_ref()?.falloff.as_ref()?),
            4 => Some(&self.light.as_ref()?.name),
            5 => Some(&self.light.as_ref()?.enabled),
            _ => Some(&self.light.as_ref()?.fields.get(index - 6)?.input),
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.select),
            1 => Some(&mut self.add),
            2 => Some(&mut self.remove),
            3 => Some(self.light.as_mut()?.falloff.as_mut()?),
            4 => Some(&mut self.light.as_mut()?.name),
            5 => Some(&mut self.light.as_mut()?.enabled),
            _ => Some(&mut self.light.as_mut()?.fields.get_mut(index - 6)?.input),
        }
    }
}
