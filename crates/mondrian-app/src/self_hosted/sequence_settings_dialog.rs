//! Self-hosted sequence settings dialog.
//!
//! The dialog owns shell-local draft state. It emits a typed sequence update
//! only on Apply, keeping editor mutations in `AppState`.

use mondrian_core::{Rational, Resolution};
use mondrian_timeline::{Sequence, SequenceSettings};
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::menu::{Dropdown, MenuItem};
use mondrian_ui_widgets::{Button, Checkbox, DialogSurface, Label, TextInput};

use crate::app::ui_actions::{
    app_shell_close_modal_action, app_shell_confirm_sequence_settings_action,
    app_shell_sequence_settings_draft_changed_action, SequenceSettingsDraftUpdatePayload,
    SequenceUpdateSettingsPayload,
};
use crate::self_hosted::icons::AppIcon;

/// Shell-local sequence settings form state.
#[derive(Debug, Clone)]
pub struct SelfHostedSequenceSettingsDraft {
    /// Sequence targeted by this modal.
    pub sequence_id: mondrian_core::types::SequenceId,
    /// Editable user-facing sequence name.
    pub name: String,
    /// Editable timeline format and preview settings.
    pub settings: SequenceSettings,
}

impl SelfHostedSequenceSettingsDraft {
    /// Build a draft from the active sequence snapshot.
    pub fn from_sequence(sequence: &Sequence) -> Self {
        Self {
            sequence_id: sequence.id,
            name: sequence.name.clone(),
            settings: sequence.settings.clone(),
        }
    }

    /// Validate the draft before applying it to editor state.
    pub fn validate(&self) -> mondrian_core::Result<()> {
        if self.name.trim().is_empty() {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "sequence_settings_draft_validate".to_owned(),
                reason: "序列名称不能为空".to_owned(),
            });
        }
        self.settings.validate()
    }

    /// Apply one shell-local form update to the real sequence settings.
    pub fn apply_update(&mut self, update: SequenceSettingsDraftUpdatePayload) {
        match update {
            SequenceSettingsDraftUpdatePayload::Name(name) => {
                self.name = name;
            }
            SequenceSettingsDraftUpdatePayload::Resolution(resolution) => {
                self.settings.resolution = resolution;
            }
            SequenceSettingsDraftUpdatePayload::FrameRate(frame_rate) => {
                if Rational::SEQUENCE_FRAME_RATES.contains(&frame_rate) {
                    self.settings.frame_rate = frame_rate;
                }
            }
            SequenceSettingsDraftUpdatePayload::AudioSampleRate(sample_rate) => {
                if SequenceSettings::AUDIO_SAMPLE_RATES.contains(&sample_rate) {
                    self.settings.audio_sample_rate = sample_rate;
                }
            }
            SequenceSettingsDraftUpdatePayload::PreviewCacheEnabled(enabled) => {
                self.settings.preview.cache_enabled = enabled;
            }
        }
        self.settings.audio_channels = self.settings.audio_channel_layout.channels();
    }

    /// Convert the current draft into the editor action payload.
    pub fn into_payload(self) -> SequenceUpdateSettingsPayload {
        SequenceUpdateSettingsPayload {
            sequence_id: self.sequence_id,
            name: self.name.trim().to_owned(),
            settings: self.settings,
        }
    }
}

const RESOLUTION_PRESETS: [(&str, Resolution); 4] = [
    ("HD 720p", Resolution::HD),
    ("Full HD 1080p", Resolution::FHD),
    ("UHD 4K", Resolution::UHD4K),
    ("DCI 4K", Resolution::DCI4K),
];

const FRAME_RATE_PRESETS: [(&str, Rational); 7] = [
    ("23.976 fps", Rational::FPS_23976),
    ("24 fps", Rational::FPS_24),
    ("25 fps", Rational::FPS_25),
    ("29.97 fps", Rational::FPS_2997),
    ("30 fps", Rational::FPS_30),
    ("50 fps", Rational::FPS_50),
    ("59.94 fps", Rational::FPS_5994),
];

const AUDIO_SAMPLE_RATE_PRESETS: [(&str, u32); 5] = [
    ("32 kHz", 32_000),
    ("44.1 kHz", 44_100),
    ("48 kHz", 48_000),
    ("88.2 kHz", 88_200),
    ("96 kHz", 96_000),
];

fn resolution_label(resolution: Resolution) -> String {
    RESOLUTION_PRESETS
        .iter()
        .find_map(|(label, preset)| (*preset == resolution).then_some((*label).to_owned()))
        .unwrap_or_else(|| resolution.to_string())
}

fn frame_rate_label(frame_rate: Rational) -> String {
    FRAME_RATE_PRESETS
        .iter()
        .find_map(|(label, preset)| (*preset == frame_rate).then_some((*label).to_owned()))
        .unwrap_or_else(|| format!("{frame_rate} fps"))
}

fn audio_sample_rate_label(sample_rate: u32) -> String {
    AUDIO_SAMPLE_RATE_PRESETS
        .iter()
        .find_map(|(label, preset)| (*preset == sample_rate).then_some((*label).to_owned()))
        .unwrap_or_else(|| format!("{sample_rate} Hz"))
}

fn resolution_items() -> Vec<MenuItem> {
    RESOLUTION_PRESETS
        .into_iter()
        .map(|(label, resolution)| {
            MenuItem::new(
                label,
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::Resolution(resolution),
                ),
            )
        })
        .collect()
}

fn frame_rate_items() -> Vec<MenuItem> {
    FRAME_RATE_PRESETS
        .into_iter()
        .map(|(label, frame_rate)| {
            MenuItem::new(
                label,
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::FrameRate(frame_rate),
                ),
            )
        })
        .collect()
}

fn audio_sample_rate_items() -> Vec<MenuItem> {
    AUDIO_SAMPLE_RATE_PRESETS
        .into_iter()
        .map(|(label, sample_rate)| {
            MenuItem::new(
                label,
                app_shell_sequence_settings_draft_changed_action(
                    SequenceSettingsDraftUpdatePayload::AudioSampleRate(sample_rate),
                ),
            )
        })
        .collect()
}

fn resolution_dropdown_for(draft: &SelfHostedSequenceSettingsDraft) -> Dropdown {
    Dropdown::new(
        resolution_label(draft.settings.resolution),
        resolution_items(),
    )
    .with_max_visible_items(4)
}

fn frame_rate_dropdown_for(draft: &SelfHostedSequenceSettingsDraft) -> Dropdown {
    Dropdown::new(
        frame_rate_label(draft.settings.frame_rate),
        frame_rate_items(),
    )
    .with_max_visible_items(7)
}

fn audio_sample_rate_dropdown_for(draft: &SelfHostedSequenceSettingsDraft) -> Dropdown {
    Dropdown::new(
        audio_sample_rate_label(draft.settings.audio_sample_rate),
        audio_sample_rate_items(),
    )
    .with_max_visible_items(5)
}

fn preview_cache_checkbox_for(draft: &SelfHostedSequenceSettingsDraft) -> Checkbox {
    Checkbox::new("Preview cache", draft.settings.preview.cache_enabled).on_change(|enabled| {
        app_shell_sequence_settings_draft_changed_action(
            SequenceSettingsDraftUpdatePayload::PreviewCacheEnabled(enabled),
        )
    })
}

const CARD_MIN_WIDTH: f32 = 340.0;
const CARD_WIDTH: f32 = 560.0;
const CARD_MIN_HEIGHT: f32 = 340.0;
const CARD_HEIGHT: f32 = 420.0;
const CONTENT_PADDING: f32 = 20.0;
const FIELD_HEIGHT: f32 = 34.0;
const DROPDOWN_HEIGHT: f32 = 28.0;
const ROW_GAP: f32 = 12.0;
const NAME_Y: f32 = 82.0;
const PRESET_ROW_Y: f32 = 142.0;
const AUDIO_ROW_Y: f32 = 202.0;
const PREVIEW_ROW_Y: f32 = 262.0;
const BUTTON_WIDTH: f32 = 88.0;
const BUTTON_HEIGHT: f32 = 32.0;
const BUTTON_GAP: f32 = 8.0;
const BUTTON_BOTTOM_INSET: f32 = 20.0;
const TITLE_FONT_SIZE: f32 = 18.0;
const LABEL_FONT_SIZE: f32 = 12.0;
const TITLE_BASELINE_Y: f32 = 22.0;
const DESCRIPTION_BASELINE_Y: f32 = 46.0;
const NAME_LABEL_BASELINE_Y: f32 = 76.0;
const PRESET_LABEL_BASELINE_Y: f32 = 136.0;
const AUDIO_LABEL_BASELINE_Y: f32 = 196.0;
const PREVIEW_LABEL_BASELINE_Y: f32 = 256.0;

/// Sequence settings modal for the self-hosted product shell.
pub struct SequenceSettingsDialog {
    id: WidgetId,
    draft: SelfHostedSequenceSettingsDraft,
    surface: DialogSurface,
    bounds: Rect,
    card: Rect,
    title_label: Label,
    description_label: Label,
    name_label: Label,
    frame_size_label: Label,
    frame_rate_label_widget: Label,
    audio_label: Label,
    preview_label: Label,
    name_input: TextInput,
    resolution_dropdown: Dropdown,
    frame_rate_dropdown: Dropdown,
    audio_sample_rate_dropdown: Dropdown,
    preview_cache_checkbox: Checkbox,
    cancel_button: Button,
    apply_button: Button,
}

impl SequenceSettingsDialog {
    /// Build the sequence settings dialog from an explicit draft.
    pub fn new(draft: SelfHostedSequenceSettingsDraft) -> Self {
        let title_label = Label::new("Sequence Settings")
            .popover_foreground()
            .with_font_size(TITLE_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let description_label =
            Label::new("Adjust timeline format and preview settings for the active sequence.")
                .muted()
                .with_font_size(LABEL_FONT_SIZE)
                .with_padding(0.0, 0.0)
                .wrapped();
        let name_label = Label::new("Name")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let frame_size_label = Label::new("Frame size")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let frame_rate_label_widget = Label::new("Frame rate")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let audio_label = Label::new("Audio")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let preview_label = Label::new("Preview")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let name_input = TextInput::new("Sequence name").with_text(&draft.name).on_change(|name| {
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::Name(name.into()),
            )
        });
        let resolution_dropdown = resolution_dropdown_for(&draft);
        let frame_rate_dropdown = frame_rate_dropdown_for(&draft);
        let audio_sample_rate_dropdown = audio_sample_rate_dropdown_for(&draft);
        let preview_cache_checkbox = preview_cache_checkbox_for(&draft);
        Self {
            id: WidgetId::new(),
            draft,
            surface: DialogSurface::new(
                Size::new(CARD_MIN_WIDTH, CARD_MIN_HEIGHT),
                Size::new(CARD_WIDTH, CARD_HEIGHT),
            )
            .with_content_padding(CONTENT_PADDING),
            bounds: Rect::ZERO,
            card: Rect::ZERO,
            title_label,
            description_label,
            name_label,
            frame_size_label,
            frame_rate_label_widget,
            audio_label,
            preview_label,
            name_input,
            resolution_dropdown,
            frame_rate_dropdown,
            audio_sample_rate_dropdown,
            preview_cache_checkbox,
            cancel_button: Button::new("Cancel").on_click(app_shell_close_modal_action()),
            apply_button: AppIcon::Save
                .text_button("Apply")
                .expect("bundled Save icon asset should parse")
                .on_click(app_shell_confirm_sequence_settings_action()),
        }
    }

    /// Apply one shell-local update and rebuild controls whose labels changed.
    pub fn apply_update(&mut self, update: SequenceSettingsDraftUpdatePayload) {
        let rebuild_controls = !matches!(update, SequenceSettingsDraftUpdatePayload::Name(_));
        self.draft.apply_update(update);
        if rebuild_controls {
            self.resolution_dropdown = resolution_dropdown_for(&self.draft);
            self.frame_rate_dropdown = frame_rate_dropdown_for(&self.draft);
            self.audio_sample_rate_dropdown = audio_sample_rate_dropdown_for(&self.draft);
            self.preview_cache_checkbox = preview_cache_checkbox_for(&self.draft);
            if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                self.layout(self.bounds);
            }
        }
    }

    /// Current shell-local draft.
    pub fn draft(&self) -> &SelfHostedSequenceSettingsDraft {
        &self.draft
    }
}

impl Widget for SequenceSettingsDialog {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        self.surface.preferred_size()
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.card = self.surface.card_rect(bounds);
        let content = self.surface.content_rect(self.card);
        self.title_label.layout(Rect::new(
            content.x,
            content.y + TITLE_BASELINE_Y,
            content.width,
            LABEL_FONT_SIZE * 2.0,
        ));
        self.description_label.layout(Rect::new(
            content.x,
            content.y + DESCRIPTION_BASELINE_Y,
            content.width,
            LABEL_FONT_SIZE * 3.0,
        ));
        self.name_label.layout(Rect::new(
            content.x,
            content.y + NAME_LABEL_BASELINE_Y,
            content.width,
            LABEL_FONT_SIZE * 1.5,
        ));
        self.name_input.layout(Rect::new(
            content.x,
            content.y + NAME_Y,
            content.width,
            FIELD_HEIGHT,
        ));

        let half = (content.width - ROW_GAP) * 0.5;
        self.frame_size_label.layout(Rect::new(
            content.x,
            content.y + PRESET_LABEL_BASELINE_Y,
            half,
            LABEL_FONT_SIZE * 1.5,
        ));
        self.frame_rate_label_widget.layout(Rect::new(
            content.x + half + ROW_GAP,
            content.y + PRESET_LABEL_BASELINE_Y,
            half,
            LABEL_FONT_SIZE * 1.5,
        ));
        self.resolution_dropdown.layout(Rect::new(
            content.x,
            content.y + PRESET_ROW_Y,
            half,
            DROPDOWN_HEIGHT,
        ));
        self.frame_rate_dropdown.layout(Rect::new(
            content.x + half + ROW_GAP,
            content.y + PRESET_ROW_Y,
            half,
            DROPDOWN_HEIGHT,
        ));
        self.audio_label.layout(Rect::new(
            content.x,
            content.y + AUDIO_LABEL_BASELINE_Y,
            half,
            LABEL_FONT_SIZE * 1.5,
        ));
        self.audio_sample_rate_dropdown.layout(Rect::new(
            content.x,
            content.y + AUDIO_ROW_Y,
            half,
            DROPDOWN_HEIGHT,
        ));
        self.preview_label.layout(Rect::new(
            content.x,
            content.y + PREVIEW_LABEL_BASELINE_Y,
            half,
            LABEL_FONT_SIZE * 1.5,
        ));
        self.preview_cache_checkbox.layout(Rect::new(
            content.x,
            content.y + PREVIEW_ROW_Y,
            half,
            DROPDOWN_HEIGHT,
        ));

        let button_y = self.card.y + self.card.height - BUTTON_BOTTOM_INSET - BUTTON_HEIGHT;
        self.cancel_button.layout(Rect::new(
            self.card.x + self.card.width - CONTENT_PADDING - BUTTON_WIDTH * 2.0 - BUTTON_GAP,
            button_y,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        ));
        self.apply_button.layout(Rect::new(
            self.card.x + self.card.width - CONTENT_PADDING - BUTTON_WIDTH,
            button_y,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        ));
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                (ctx.dispatch)(app_shell_close_modal_action());
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Enter, .. } => {
                (ctx.dispatch)(app_shell_confirm_sequence_settings_action());
                return EventResult::Handled;
            }
            UiEvent::MouseDown { position, .. }
                if self.surface.is_outside_card(self.card, *position) =>
            {
                return EventResult::Handled;
            }
            _ => {}
        }

        if self.apply_button.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        if self.cancel_button.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        if self.name_input.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        if self.resolution_dropdown.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        if self.frame_rate_dropdown.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        if self.audio_sample_rate_dropdown.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        if self.preview_cache_checkbox.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.surface.paint(self.bounds, self.card, ctx);
        self.title_label.paint(ctx);
        self.description_label.paint(ctx);
        self.name_label.paint(ctx);
        self.name_input.paint(ctx);
        self.frame_size_label.paint(ctx);
        self.frame_rate_label_widget.paint(ctx);
        self.resolution_dropdown.paint(ctx);
        self.frame_rate_dropdown.paint(ctx);
        self.audio_label.paint(ctx);
        self.audio_sample_rate_dropdown.paint(ctx);
        self.preview_label.paint(ctx);
        self.preview_cache_checkbox.paint(ctx);
        self.cancel_button.paint(ctx);
        self.apply_button.paint(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        14
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.title_label),
            1 => Some(&self.description_label),
            2 => Some(&self.name_label),
            3 => Some(&self.frame_size_label),
            4 => Some(&self.frame_rate_label_widget),
            5 => Some(&self.audio_label),
            6 => Some(&self.preview_label),
            7 => Some(&self.name_input),
            8 => Some(&self.resolution_dropdown),
            9 => Some(&self.frame_rate_dropdown),
            10 => Some(&self.audio_sample_rate_dropdown),
            11 => Some(&self.preview_cache_checkbox),
            12 => Some(&self.cancel_button),
            13 => Some(&self.apply_button),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.title_label),
            1 => Some(&mut self.description_label),
            2 => Some(&mut self.name_label),
            3 => Some(&mut self.frame_size_label),
            4 => Some(&mut self.frame_rate_label_widget),
            5 => Some(&mut self.audio_label),
            6 => Some(&mut self.preview_label),
            7 => Some(&mut self.name_input),
            8 => Some(&mut self.resolution_dropdown),
            9 => Some(&mut self.frame_rate_dropdown),
            10 => Some(&mut self.audio_sample_rate_dropdown),
            11 => Some(&mut self.preview_cache_checkbox),
            12 => Some(&mut self.cancel_button),
            13 => Some(&mut self.apply_button),
            _ => None,
        }
    }
}
