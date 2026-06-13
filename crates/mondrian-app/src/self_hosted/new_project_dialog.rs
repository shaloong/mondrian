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
use mondrian_ui_widgets::{Button, Checkbox, TextInput};

use crate::app::ui_actions::{
    app_shell_cancel_new_project_dialog_action, app_shell_confirm_new_project_dialog_action,
    app_shell_new_project_draft_changed_action, NewProjectDraftUpdatePayload,
    ProjectCreateWithSettingsPayload,
};
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
            name: "Untitled".into(),
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
            "Untitled".into()
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
        format!("Untitled.{PROJECT_FILE_EXTENSION}")
    } else {
        format!("{stem}.{PROJECT_FILE_EXTENSION}")
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

const AUDIO_SAMPLE_RATE_PRESETS: [(&str, u32); 3] =
    [("44.1 kHz", 44_100), ("48 kHz", 48_000), ("96 kHz", 96_000)];

fn project_name_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .map(|stem| stem.trim().to_string())
        .unwrap_or_else(|| "Untitled".to_string())
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
    Checkbox::new("Create proxies", draft.project_settings.proxy_enabled).on_change(|enabled| {
        app_shell_new_project_draft_changed_action(NewProjectDraftUpdatePayload::ProxyEnabled(
            enabled,
        ))
    })
}

fn preview_cache_checkbox_for(draft: &SelfHostedNewProjectDraft) -> Checkbox {
    Checkbox::new(
        "Preview cache",
        draft.sequence_settings.preview.cache_enabled,
    )
    .on_change(|enabled| {
        app_shell_new_project_draft_changed_action(
            NewProjectDraftUpdatePayload::PreviewCacheEnabled(enabled),
        )
    })
}

pub struct NewProjectDialog {
    id: WidgetId,
    draft: SelfHostedNewProjectDraft,
    bounds: Rect,
    card: Rect,
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
        let name_input = TextInput::new("Project name").with_text(&draft.name).on_change(|name| {
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
            bounds: Rect::ZERO,
            card: Rect::ZERO,
            name_input,
            resolution_dropdown,
            frame_rate_dropdown,
            audio_sample_rate_dropdown,
            proxy_checkbox,
            preview_cache_checkbox,
            cancel_button: Button::new("Cancel")
                .on_click(app_shell_cancel_new_project_dialog_action()),
            create_button: Button::new("Create...")
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
        Size::new(520.0, 390.0)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let card_width = bounds.width.clamp(320.0, 520.0);
        let card_height = bounds.height.clamp(320.0, 390.0);
        self.card = Rect::new(
            bounds.x + (bounds.width - card_width) * 0.5,
            bounds.y + (bounds.height - card_height) * 0.5,
            card_width,
            card_height,
        );
        let content = self.card.inset(20.0, 20.0);
        self.name_input
            .layout(Rect::new(content.x, content.y + 82.0, content.width, 34.0));
        let row_gap = 12.0;
        let half = (content.width - row_gap) * 0.5;
        self.resolution_dropdown
            .layout(Rect::new(content.x, content.y + 142.0, half, 28.0));
        self.frame_rate_dropdown.layout(Rect::new(
            content.x + half + row_gap,
            content.y + 142.0,
            half,
            28.0,
        ));
        self.audio_sample_rate_dropdown
            .layout(Rect::new(content.x, content.y + 202.0, half, 28.0));
        self.proxy_checkbox.layout(Rect::new(content.x, content.y + 254.0, half, 28.0));
        self.preview_cache_checkbox.layout(Rect::new(
            content.x + half + row_gap,
            content.y + 254.0,
            half,
            28.0,
        ));

        let button_y = self.card.y + self.card.height - 52.0;
        self.cancel_button.layout(Rect::new(
            self.card.x + self.card.width - 204.0,
            button_y,
            88.0,
            32.0,
        ));
        self.create_button.layout(Rect::new(
            self.card.x + self.card.width - 108.0,
            button_y,
            88.0,
            32.0,
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
            UiEvent::MouseDown { position, .. } if !self.card.contains(*position) => {
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
        ctx.encoder.draw_rect(
            self.bounds,
            mondrian_core::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.38 },
            0.0,
        );
        ctx.encoder.draw_rect(self.card, ctx.theme.colors.card, 8.0);
        let top_left = Point::new(self.card.x, self.card.y);
        let top_right = Point::new(self.card.x + self.card.width, self.card.y);
        let bottom_left = Point::new(self.card.x, self.card.y + self.card.height);
        let bottom_right = Point::new(
            self.card.x + self.card.width,
            self.card.y + self.card.height,
        );
        ctx.encoder.draw_line(top_left, top_right, 1.0, ctx.theme.colors.border);
        ctx.encoder.draw_line(bottom_left, bottom_right, 1.0, ctx.theme.colors.border);
        ctx.encoder.draw_line(top_left, bottom_left, 1.0, ctx.theme.colors.border);
        ctx.encoder.draw_line(top_right, bottom_right, 1.0, ctx.theme.colors.border);

        let content = self.card.inset(20.0, 20.0);
        ctx.encoder.draw_text(
            "New Project",
            18.0,
            Point::new(content.x, content.y + 22.0),
            ctx.theme.colors.foreground,
        );
        ctx.encoder.draw_text_box(
            "Create a project file and initialize the timeline with default production settings.",
            12.0,
            Point::new(content.x, content.y + 46.0),
            content.width,
            ctx.theme.colors.muted_foreground,
        );
        ctx.encoder.draw_text(
            "Name",
            12.0,
            Point::new(content.x, content.y + 76.0),
            ctx.theme.colors.muted_foreground,
        );
        self.name_input.paint(ctx);
        let row_gap = 12.0;
        let half = (content.width - row_gap) * 0.5;
        ctx.encoder.draw_text(
            "Frame size",
            12.0,
            Point::new(content.x, content.y + 136.0),
            ctx.theme.colors.muted_foreground,
        );
        ctx.encoder.draw_text(
            "Frame rate",
            12.0,
            Point::new(content.x + half + row_gap, content.y + 136.0),
            ctx.theme.colors.muted_foreground,
        );
        self.resolution_dropdown.paint(ctx);
        self.frame_rate_dropdown.paint(ctx);
        ctx.encoder.draw_text(
            "Audio",
            12.0,
            Point::new(content.x, content.y + 196.0),
            ctx.theme.colors.muted_foreground,
        );
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
        8
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.name_input),
            1 => Some(&self.resolution_dropdown),
            2 => Some(&self.frame_rate_dropdown),
            3 => Some(&self.audio_sample_rate_dropdown),
            4 => Some(&self.proxy_checkbox),
            5 => Some(&self.preview_cache_checkbox),
            6 => Some(&self.cancel_button),
            7 => Some(&self.create_button),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.name_input),
            1 => Some(&mut self.resolution_dropdown),
            2 => Some(&mut self.frame_rate_dropdown),
            3 => Some(&mut self.audio_sample_rate_dropdown),
            4 => Some(&mut self.proxy_checkbox),
            5 => Some(&mut self.preview_cache_checkbox),
            6 => Some(&mut self.cancel_button),
            7 => Some(&mut self.create_button),
            _ => None,
        }
    }
}
