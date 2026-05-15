use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutAction {
    // ── File (editable) ──
    ImportMedia,
    OpenProject,
    SaveProject,
    SaveProjectAs,
    CloseProject,
    QuitApp,
    // ── Transport (non-editable) ──
    PlayPause,
    ShuttleBack,
    Pause,
    ShuttleForward,
    StepBack,
    StepForward,
    JumpStart,
    JumpEnd,
    MarkIn,
    MarkOut,
    // ── Tools (non-editable) ──
    SelectTool,
    BladeTool,
}

impl ShortcutAction {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::ImportMedia => "导入素材",
            Self::OpenProject => "打开项目",
            Self::SaveProject => "保存项目",
            Self::SaveProjectAs => "另存为",
            Self::CloseProject => "关闭项目",
            Self::QuitApp => "退出",
            Self::PlayPause => "播放/暂停",
            Self::ShuttleBack => "倒放",
            Self::Pause => "暂停",
            Self::ShuttleForward => "快进",
            Self::StepBack => "后退一帧",
            Self::StepForward => "前进一帧",
            Self::JumpStart => "跳到开头",
            Self::JumpEnd => "跳到结尾",
            Self::MarkIn => "标记入点",
            Self::MarkOut => "标记出点",
            Self::SelectTool => "选择工具",
            Self::BladeTool => "剪刀工具",
        }
    }

    /// Whether this shortcut can be customized in Preferences.
    pub fn is_editable(self) -> bool {
        matches!(
            self,
            Self::ImportMedia
                | Self::OpenProject
                | Self::SaveProject
                | Self::SaveProjectAs
                | Self::CloseProject
                | Self::QuitApp
        )
    }

    /// Hardcoded default binding. Returns `None` only for editable actions
    /// whose default is unset (handled by `ShortcutPreferences`).
    pub fn hardcoded_binding(self) -> Option<ShortcutBinding> {
        match self {
            // Transport
            Self::PlayPause => Some(ShortcutBinding::key(ShortcutKey::Space)),
            Self::ShuttleBack => Some(ShortcutBinding::key(ShortcutKey::J)),
            Self::Pause => Some(ShortcutBinding::key(ShortcutKey::K)),
            Self::ShuttleForward => Some(ShortcutBinding::key(ShortcutKey::L)),
            Self::StepBack => Some(ShortcutBinding::key(ShortcutKey::ArrowLeft)),
            Self::StepForward => Some(ShortcutBinding::key(ShortcutKey::ArrowRight)),
            Self::JumpStart => Some(ShortcutBinding::key(ShortcutKey::Home)),
            Self::JumpEnd => Some(ShortcutBinding::key(ShortcutKey::End)),
            Self::MarkIn => Some(ShortcutBinding::key(ShortcutKey::I)),
            Self::MarkOut => Some(ShortcutBinding::key(ShortcutKey::O)),
            // Tools
            Self::SelectTool => Some(ShortcutBinding::key(ShortcutKey::V)),
            Self::BladeTool => Some(ShortcutBinding::key(ShortcutKey::B)),
            // Editable: delegate to preferences
            _ => None,
        }
    }

    /// All non-editable actions (for conflict detection).
    pub fn non_editable_actions() -> Vec<Self> {
        use ShortcutAction::*;
        vec![
            PlayPause, ShuttleBack, Pause, ShuttleForward, StepBack, StepForward,
            JumpStart, JumpEnd, MarkIn, MarkOut, SelectTool, BladeTool,
        ]
    }

    /// Editable actions shown in preferences.
    pub fn editable_actions() -> Vec<Self> {
        use ShortcutAction::*;
        vec![ImportMedia, OpenProject, SaveProject, SaveProjectAs, CloseProject, QuitApp]
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ShortcutKey {
    A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    Num0, Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9,
    F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12,
    Enter, Space, Delete,
    ArrowLeft, ArrowRight, ArrowUp, ArrowDown,
    Home, End,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShortcutBinding {
    pub command: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: ShortcutKey,
}

impl ShortcutBinding {
    pub fn key(key: ShortcutKey) -> Self {
        Self { command: false, shift: false, alt: false, key }
    }

    pub fn matches_input(&self, input: &egui::InputState) -> bool {
        if input.modifiers.command != self.command {
            return false;
        }
        if input.modifiers.shift != self.shift {
            return false;
        }
        if input.modifiers.alt != self.alt {
            return false;
        }
        input.key_pressed(self.key.to_egui())
    }

    pub fn display_text(&self) -> String {
        let mut parts = Vec::new();
        if self.command {
            parts.push("Ctrl".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        parts.push(self.key.label().to_string());
        parts.join("+")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShortcutPreferences {
    pub import_media: Option<ShortcutBinding>,
    pub open_project: Option<ShortcutBinding>,
    pub save_project: Option<ShortcutBinding>,
    pub save_project_as: Option<ShortcutBinding>,
    pub close_project: Option<ShortcutBinding>,
    pub quit_app: Option<ShortcutBinding>,
}

impl Default for ShortcutPreferences {
    fn default() -> Self {
        Self {
            import_media: Some(ShortcutBinding {
                command: true,
                shift: false,
                alt: false,
                key: ShortcutKey::I,
            }),
            open_project: Some(ShortcutBinding {
                command: true,
                shift: false,
                alt: false,
                key: ShortcutKey::O,
            }),
            save_project: Some(ShortcutBinding {
                command: true,
                shift: false,
                alt: false,
                key: ShortcutKey::S,
            }),
            save_project_as: Some(ShortcutBinding {
                command: true,
                shift: true,
                alt: false,
                key: ShortcutKey::S,
            }),
            close_project: Some(ShortcutBinding {
                command: true,
                shift: false,
                alt: false,
                key: ShortcutKey::W,
            }),
            quit_app: Some(ShortcutBinding {
                command: true,
                shift: false,
                alt: false,
                key: ShortcutKey::Q,
            }),
        }
    }
}

impl ShortcutPreferences {
    pub fn import_media_label(&self) -> String {
        self.import_media
            .as_ref()
            .map(|s| s.display_text())
            .unwrap_or_else(|| "未设置".to_string())
    }

    pub fn open_project_label(&self) -> String {
        self.open_project
            .as_ref()
            .map(|s| s.display_text())
            .unwrap_or_else(|| "未设置".to_string())
    }

    pub fn save_project_label(&self) -> String {
        self.save_project
            .as_ref()
            .map(|s| s.display_text())
            .unwrap_or_else(|| "未设置".to_string())
    }

    pub fn save_project_as_label(&self) -> String {
        self.save_project_as
            .as_ref()
            .map(|s| s.display_text())
            .unwrap_or_else(|| "未设置".to_string())
    }

    pub fn close_project_label(&self) -> String {
        self.close_project
            .as_ref()
            .map(|s| s.display_text())
            .unwrap_or_else(|| "未设置".to_string())
    }

    pub fn quit_app_label(&self) -> String {
        self.quit_app
            .as_ref()
            .map(|s| s.display_text())
            .unwrap_or_else(|| "未设置".to_string())
    }
}

impl ShortcutKey {
    pub fn label(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
            Self::E => "E",
            Self::F => "F",
            Self::G => "G",
            Self::H => "H",
            Self::I => "I",
            Self::J => "J",
            Self::K => "K",
            Self::L => "L",
            Self::M => "M",
            Self::N => "N",
            Self::O => "O",
            Self::P => "P",
            Self::Q => "Q",
            Self::R => "R",
            Self::S => "S",
            Self::T => "T",
            Self::U => "U",
            Self::V => "V",
            Self::W => "W",
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
            Self::Num0 => "0",
            Self::Num1 => "1",
            Self::Num2 => "2",
            Self::Num3 => "3",
            Self::Num4 => "4",
            Self::Num5 => "5",
            Self::Num6 => "6",
            Self::Num7 => "7",
            Self::Num8 => "8",
            Self::Num9 => "9",
            Self::F1 => "F1",
            Self::F2 => "F2",
            Self::F3 => "F3",
            Self::F4 => "F4",
            Self::F5 => "F5",
            Self::F6 => "F6",
            Self::F7 => "F7",
            Self::F8 => "F8",
            Self::F9 => "F9",
            Self::F10 => "F10",
            Self::F11 => "F11",
            Self::F12 => "F12",
            Self::Enter => "Enter",
            Self::Space => "Space",
            Self::Delete => "Delete",
            Self::ArrowLeft => "←",
            Self::ArrowRight => "→",
            Self::ArrowUp => "↑",
            Self::ArrowDown => "↓",
            Self::Home => "Home",
            Self::End => "End",
        }
    }

    pub fn to_egui(self) -> egui::Key {
        match self {
            Self::A => egui::Key::A,
            Self::B => egui::Key::B,
            Self::C => egui::Key::C,
            Self::D => egui::Key::D,
            Self::E => egui::Key::E,
            Self::F => egui::Key::F,
            Self::G => egui::Key::G,
            Self::H => egui::Key::H,
            Self::I => egui::Key::I,
            Self::J => egui::Key::J,
            Self::K => egui::Key::K,
            Self::L => egui::Key::L,
            Self::M => egui::Key::M,
            Self::N => egui::Key::N,
            Self::O => egui::Key::O,
            Self::P => egui::Key::P,
            Self::Q => egui::Key::Q,
            Self::R => egui::Key::R,
            Self::S => egui::Key::S,
            Self::T => egui::Key::T,
            Self::U => egui::Key::U,
            Self::V => egui::Key::V,
            Self::W => egui::Key::W,
            Self::X => egui::Key::X,
            Self::Y => egui::Key::Y,
            Self::Z => egui::Key::Z,
            Self::Num0 => egui::Key::Num0,
            Self::Num1 => egui::Key::Num1,
            Self::Num2 => egui::Key::Num2,
            Self::Num3 => egui::Key::Num3,
            Self::Num4 => egui::Key::Num4,
            Self::Num5 => egui::Key::Num5,
            Self::Num6 => egui::Key::Num6,
            Self::Num7 => egui::Key::Num7,
            Self::Num8 => egui::Key::Num8,
            Self::Num9 => egui::Key::Num9,
            Self::F1 => egui::Key::F1,
            Self::F2 => egui::Key::F2,
            Self::F3 => egui::Key::F3,
            Self::F4 => egui::Key::F4,
            Self::F5 => egui::Key::F5,
            Self::F6 => egui::Key::F6,
            Self::F7 => egui::Key::F7,
            Self::F8 => egui::Key::F8,
            Self::F9 => egui::Key::F9,
            Self::F10 => egui::Key::F10,
            Self::F11 => egui::Key::F11,
            Self::F12 => egui::Key::F12,
            Self::Enter => egui::Key::Enter,
            Self::Space => egui::Key::Space,
            Self::Delete => egui::Key::Delete,
            Self::ArrowLeft => egui::Key::ArrowLeft,
            Self::ArrowRight => egui::Key::ArrowRight,
            Self::ArrowUp => egui::Key::ArrowUp,
            Self::ArrowDown => egui::Key::ArrowDown,
            Self::Home => egui::Key::Home,
            Self::End => egui::Key::End,
        }
    }

    pub fn from_egui(key: egui::Key) -> Option<Self> {
        Some(match key {
            egui::Key::A => Self::A,
            egui::Key::B => Self::B,
            egui::Key::C => Self::C,
            egui::Key::D => Self::D,
            egui::Key::E => Self::E,
            egui::Key::F => Self::F,
            egui::Key::G => Self::G,
            egui::Key::H => Self::H,
            egui::Key::I => Self::I,
            egui::Key::J => Self::J,
            egui::Key::K => Self::K,
            egui::Key::L => Self::L,
            egui::Key::M => Self::M,
            egui::Key::N => Self::N,
            egui::Key::O => Self::O,
            egui::Key::P => Self::P,
            egui::Key::Q => Self::Q,
            egui::Key::R => Self::R,
            egui::Key::S => Self::S,
            egui::Key::T => Self::T,
            egui::Key::U => Self::U,
            egui::Key::V => Self::V,
            egui::Key::W => Self::W,
            egui::Key::X => Self::X,
            egui::Key::Y => Self::Y,
            egui::Key::Z => Self::Z,
            egui::Key::Num0 => Self::Num0,
            egui::Key::Num1 => Self::Num1,
            egui::Key::Num2 => Self::Num2,
            egui::Key::Num3 => Self::Num3,
            egui::Key::Num4 => Self::Num4,
            egui::Key::Num5 => Self::Num5,
            egui::Key::Num6 => Self::Num6,
            egui::Key::Num7 => Self::Num7,
            egui::Key::Num8 => Self::Num8,
            egui::Key::Num9 => Self::Num9,
            egui::Key::F1 => Self::F1,
            egui::Key::F2 => Self::F2,
            egui::Key::F3 => Self::F3,
            egui::Key::F4 => Self::F4,
            egui::Key::F5 => Self::F5,
            egui::Key::F6 => Self::F6,
            egui::Key::F7 => Self::F7,
            egui::Key::F8 => Self::F8,
            egui::Key::F9 => Self::F9,
            egui::Key::F10 => Self::F10,
            egui::Key::F11 => Self::F11,
            egui::Key::F12 => Self::F12,
            egui::Key::Enter => Self::Enter,
            egui::Key::Space => Self::Space,
            egui::Key::Delete => Self::Delete,
            egui::Key::ArrowLeft => Self::ArrowLeft,
            egui::Key::ArrowRight => Self::ArrowRight,
            egui::Key::ArrowUp => Self::ArrowUp,
            egui::Key::ArrowDown => Self::ArrowDown,
            egui::Key::Home => Self::Home,
            egui::Key::End => Self::End,
            _ => return None,
        })
    }
}
