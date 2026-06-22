//! Self-hosted new-project dialog and draft state.
//!
//! The dialog owns shell-local form state. It edits the same project and
//! sequence settings structs consumed by `AppState`, then emits a concrete
//! project creation payload only when the user confirms.

use std::path::Path;

use mondrian_core::{ProjectSettings, Rational, Resolution};
use mondrian_timeline::SequenceSettings;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::menu::{Dropdown, MenuItem};
use mondrian_ui_widgets::{Button, Checkbox, DialogSurface, Label, TextInput};

use crate::app::ui_actions::{
    app_shell_cancel_new_project_dialog_action, app_shell_confirm_new_project_dialog_action,
    app_shell_new_project_draft_changed_action, NewProjectDraftUpdatePayload,
    ProjectCreateWithSettingsPayload,
};
use crate::self_hosted::icons::AppIcon;
use crate::self_hosted::shell::PROJECT_FILE_EXTENSION;

/// Self-hosted new-project form state.
///
/// The draft deliberately carries the same settings structs consumed by
/// `AppState` so form widgets edit the eventual creation payload instead of a
/// parallel DTO that can drift from project lifecycle semantics.
#[derive(Debug, Clone)]
pub struct SelfHostedNewProjectDraft {
    pub name: String,
    pub sequence_settings: SequenceSettings,
    pub project_settings: ProjectSettings,
}

impl Default for SelfHostedNewProjectDraft {
    fn default() -> Self {
        Self {
            name: "未命名".into(),
            sequence_settings: SequenceSettings::default(),
            project_settings: ProjectSettings::default(),
        }
    }
}

impl SelfHostedNewProjectDraft {
    /// Build a draft using the file stem as the initial project name.
    pub fn from_project_path(path: &Path) -> Self {
        Self {
            name: project_name_from_path(path),
            ..Self::default()
        }
    }

    /// Validate the draft before turning it into a creation payload.
    pub fn validate(&self) -> mondrian_core::Result<()> {
        self.sequence_settings.validate()
    }

    /// Apply one shell-local form update to the real creation settings.
    pub fn apply_update(&mut self, update: NewProjectDraftUpdatePayload) {
        match update {
            NewProjectDraftUpdatePayload::Name(name) => {
                self.name = name;
            }
            NewProjectDraftUpdatePayload::Resolution(resolution) => {
                self.sequence_settings.resolution = resolution;
            }
            NewProjectDraftUpdatePayload::FrameRate(frame_rate) => {
                if Rational::SEQUENCE_FRAME_RATES.contains(&frame_rate) {
                    self.sequence_settings.frame_rate = frame_rate;
                }
            }
            NewProjectDraftUpdatePayload::AudioSampleRate(sample_rate) => {
                if SequenceSettings::AUDIO_SAMPLE_RATES.contains(&sample_rate) {
                    self.sequence_settings.audio_sample_rate = sample_rate;
                }
            }
            NewProjectDraftUpdatePayload::ProxyEnabled(enabled) => {
                self.project_settings.proxy_enabled = enabled;
            }
            NewProjectDraftUpdatePayload::PreviewCacheEnabled(enabled) => {
                self.sequence_settings.preview.cache_enabled = enabled;
            }
        }
    }

    fn display_name(&self) -> String {
        let name = self.name.trim();
        if name.is_empty() {
            "未命名".into()
        } else {
            name.into()
        }
    }

    /// Convert the current form state into the action payload consumed by
    /// `AppState`.
    pub fn into_payload(
        self,
        project_file: impl Into<std::path::PathBuf>,
    ) -> ProjectCreateWithSettingsPayload {
        ProjectCreateWithSettingsPayload {
            project_file: project_file.into(),
            name: self.display_name(),
            sequence_settings: self.sequence_settings,
            project_settings: self.project_settings,
        }
    }
}

/// Build a filesystem-safe default project filename from a user-facing name.
pub fn default_project_file_name(name: &str) -> String {
    let stem: String = name
        .trim()
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect();
    let stem = stem.trim_matches(['.', ' ']).trim();
    if stem.is_empty() {
        format!("未命名.{PROJECT_FILE_EXTENSION}")
    } else {
        format!("{stem}.{PROJECT_FILE_EXTENSION}")
    }
}

const RESOLUTION_PRESETS: [(&str, Resolution); 4] = [
    ("高清 720p", Resolution::HD),
    ("全高清 1080p", Resolution::FHD),
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

const AUDIO_SAMPLE_RATE_PRESETS: [(&str, u32); 3] =
    [("44.1 kHz", 44_100), ("48 kHz", 48_000), ("96 kHz", 96_000)];

fn project_name_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .map(|stem| stem.trim().to_string())
        .unwrap_or_else(|| "未命名".to_string())
}

fn resolution_label(resolution: Resolution) -> String {
    RESOLUTION_PRESETS
        .iter()
        .find_map(|(label, preset)| (*preset == resolution).then_some((*label).to_string()))
        .unwrap_or_else(|| resolution.to_string())
}

fn frame_rate_label(frame_rate: Rational) -> String {
    FRAME_RATE_PRESETS
        .iter()
        .find_map(|(label, preset)| (*preset == frame_rate).then_some((*label).to_string()))
        .unwrap_or_else(|| format!("{frame_rate} fps"))
}

fn audio_sample_rate_label(sample_rate: u32) -> String {
    AUDIO_SAMPLE_RATE_PRESETS
        .iter()
        .find_map(|(label, preset)| (*preset == sample_rate).then_some((*label).to_string()))
        .unwrap_or_else(|| format!("{} Hz", sample_rate))
}

fn new_project_resolution_items() -> Vec<MenuItem> {
    RESOLUTION_PRESETS
        .into_iter()
        .map(|(label, resolution)| {
            MenuItem::new(
                label,
                app_shell_new_project_draft_changed_action(
                    NewProjectDraftUpdatePayload::Resolution(resolution),
                ),
            )
        })
        .collect()
}

fn new_project_frame_rate_items() -> Vec<MenuItem> {
    FRAME_RATE_PRESETS
        .into_iter()
        .map(|(label, frame_rate)| {
            MenuItem::new(
                label,
                app_shell_new_project_draft_changed_action(
                    NewProjectDraftUpdatePayload::FrameRate(frame_rate),
                ),
            )
        })
        .collect()
}

fn new_project_audio_sample_rate_items() -> Vec<MenuItem> {
    AUDIO_SAMPLE_RATE_PRESETS
        .into_iter()
        .map(|(label, sample_rate)| {
            MenuItem::new(
                label,
                app_shell_new_project_draft_changed_action(
                    NewProjectDraftUpdatePayload::AudioSampleRate(sample_rate),
                ),
            )
        })
        .collect()
}

fn resolution_dropdown_for(draft: &SelfHostedNewProjectDraft) -> Dropdown {
    Dropdown::new(
        resolution_label(draft.sequence_settings.resolution),
        new_project_resolution_items(),
    )
    .with_max_visible_items(4)
}

fn frame_rate_dropdown_for(draft: &SelfHostedNewProjectDraft) -> Dropdown {
    Dropdown::new(
        frame_rate_label(draft.sequence_settings.frame_rate),
        new_project_frame_rate_items(),
    )
    .with_max_visible_items(7)
}

fn audio_sample_rate_dropdown_for(draft: &SelfHostedNewProjectDraft) -> Dropdown {
    Dropdown::new(
        audio_sample_rate_label(draft.sequence_settings.audio_sample_rate),
        new_project_audio_sample_rate_items(),
    )
    .with_max_visible_items(3)
}

fn proxy_checkbox_for(draft: &SelfHostedNewProjectDraft) -> Checkbox {
    Checkbox::new("创建代理", draft.project_settings.proxy_enabled).on_change(|enabled| {
        app_shell_new_project_draft_changed_action(NewProjectDraftUpdatePayload::ProxyEnabled(
            enabled,
        ))
    })
}

fn preview_cache_checkbox_for(draft: &SelfHostedNewProjectDraft) -> Checkbox {
    Checkbox::new("预览缓存", draft.sequence_settings.preview.cache_enabled).on_change(|enabled| {
        app_shell_new_project_draft_changed_action(
            NewProjectDraftUpdatePayload::PreviewCacheEnabled(enabled),
        )
    })
}

const CARD_MIN_WIDTH: f32 = 320.0;
const CARD_WIDTH: f32 = 520.0;
const CARD_MIN_HEIGHT: f32 = 320.0;
const CARD_HEIGHT: f32 = 390.0;
const CONTENT_PADDING: f32 = 20.0;
const FIELD_HEIGHT: f32 = 34.0;
const DROPDOWN_HEIGHT: f32 = 28.0;
const ROW_GAP: f32 = 12.0;
const NAME_Y: f32 = 82.0;
const PRESET_ROW_Y: f32 = 142.0;
const AUDIO_ROW_Y: f32 = 202.0;
const CHECKBOX_ROW_Y: f32 = 254.0;
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

pub struct NewProjectDialog {
    id: WidgetId,
    draft: SelfHostedNewProjectDraft,
    surface: DialogSurface,
    bounds: Rect,
    card: Rect,
    title_label: Label,
    description_label: Label,
    name_label: Label,
    frame_size_label: Label,
    frame_rate_label_widget: Label,
    audio_label: Label,
    name_input: TextInput,
    resolution_dropdown: Dropdown,
    frame_rate_dropdown: Dropdown,
    audio_sample_rate_dropdown: Dropdown,
    proxy_checkbox: Checkbox,
    preview_cache_checkbox: Checkbox,
    cancel_button: Button,
    create_button: Button,
}

impl NewProjectDialog {
    pub fn new(draft: SelfHostedNewProjectDraft) -> Self {
        let title_label = Label::new("新建项目")
            .popover_foreground()
            .with_font_size(TITLE_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let description_label = Label::new("创建项目文件，并用默认制作设置初始化时间线。")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0)
            .wrapped();
        let name_label = Label::new("名称")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let frame_size_label = Label::new("画面尺寸")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let frame_rate_label_widget = Label::new("帧率")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let audio_label = Label::new("音频")
            .muted()
            .with_font_size(LABEL_FONT_SIZE)
            .with_padding(0.0, 0.0);
        let name_input = TextInput::new("项目名称").with_text(&draft.name).on_change(|name| {
            app_shell_new_project_draft_changed_action(NewProjectDraftUpdatePayload::Name(
                name.into(),
            ))
        });
        let resolution_dropdown = resolution_dropdown_for(&draft);
        let frame_rate_dropdown = frame_rate_dropdown_for(&draft);
        let audio_sample_rate_dropdown = audio_sample_rate_dropdown_for(&draft);
        let proxy_checkbox = proxy_checkbox_for(&draft);
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
            name_input,
            resolution_dropdown,
            frame_rate_dropdown,
            audio_sample_rate_dropdown,
            proxy_checkbox,
            preview_cache_checkbox,
            cancel_button: Button::new("取消")
                .on_click(app_shell_cancel_new_project_dialog_action()),
            create_button: AppIcon::PlusFilled
                .text_button_or_label("创建...")
                .on_click(app_shell_confirm_new_project_dialog_action()),
        }
    }

    pub fn apply_update(&mut self, update: NewProjectDraftUpdatePayload) {
        let rebuild_controls = !matches!(update, NewProjectDraftUpdatePayload::Name(_));
        self.draft.apply_update(update);
        if rebuild_controls {
            self.resolution_dropdown = resolution_dropdown_for(&self.draft);
            self.frame_rate_dropdown = frame_rate_dropdown_for(&self.draft);
            self.audio_sample_rate_dropdown = audio_sample_rate_dropdown_for(&self.draft);
            self.proxy_checkbox = proxy_checkbox_for(&self.draft);
            self.preview_cache_checkbox = preview_cache_checkbox_for(&self.draft);
            if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                self.layout(self.bounds);
            }
        }
    }

    pub fn draft(&self) -> &SelfHostedNewProjectDraft {
        &self.draft
    }
}

impl Widget for NewProjectDialog {
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
        self.audio_sample_rate_dropdown.layout(Rect::new(
            content.x,
            content.y + AUDIO_ROW_Y,
            half,
            DROPDOWN_HEIGHT,
        ));
        self.audio_label.layout(Rect::new(
            content.x,
            content.y + AUDIO_LABEL_BASELINE_Y,
            half,
            LABEL_FONT_SIZE * 1.5,
        ));
        self.proxy_checkbox.layout(Rect::new(
            content.x,
            content.y + CHECKBOX_ROW_Y,
            half,
            DROPDOWN_HEIGHT,
        ));
        self.preview_cache_checkbox.layout(Rect::new(
            content.x + half + ROW_GAP,
            content.y + CHECKBOX_ROW_Y,
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
        self.create_button.layout(Rect::new(
            self.card.x + self.card.width - CONTENT_PADDING - BUTTON_WIDTH,
            button_y,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        ));
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                (ctx.dispatch)(app_shell_cancel_new_project_dialog_action());
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Enter, .. } => {
                (ctx.dispatch)(app_shell_confirm_new_project_dialog_action());
                return EventResult::Handled;
            }
            UiEvent::MouseDown { position, .. }
                if self.surface.is_outside_card(self.card, *position) =>
            {
                return EventResult::Handled;
            }
            _ => {}
        }

        if self.create_button.event(event, ctx) == EventResult::Handled {
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
        if self.proxy_checkbox.event(event, ctx) == EventResult::Handled {
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
        self.proxy_checkbox.paint(ctx);
        self.preview_cache_checkbox.paint(ctx);
        self.cancel_button.paint(ctx);
        self.create_button.paint(ctx);
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
            6 => Some(&self.name_input),
            7 => Some(&self.resolution_dropdown),
            8 => Some(&self.frame_rate_dropdown),
            9 => Some(&self.audio_sample_rate_dropdown),
            10 => Some(&self.proxy_checkbox),
            11 => Some(&self.preview_cache_checkbox),
            12 => Some(&self.cancel_button),
            13 => Some(&self.create_button),
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
            6 => Some(&mut self.name_input),
            7 => Some(&mut self.resolution_dropdown),
            8 => Some(&mut self.frame_rate_dropdown),
            9 => Some(&mut self.audio_sample_rate_dropdown),
            10 => Some(&mut self.proxy_checkbox),
            11 => Some(&mut self.preview_cache_checkbox),
            12 => Some(&mut self.cancel_button),
            13 => Some(&mut self.create_button),
            _ => None,
        }
    }
}
