//! Pure platform modifier semantics shared by single-line and multiline inputs.

use mondrian_ui_core::types::Modifiers;

// ── Platform Keymap ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlatformKeymap {
    Windows,
    Linux,
    MacOs,
}

impl PlatformKeymap {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            PlatformKeymap::MacOs
        } else if cfg!(target_os = "linux") {
            PlatformKeymap::Linux
        } else {
            PlatformKeymap::Windows
        }
    }

    /// Whether modifiers represent "exact primary" (primary only, no alt/shift/secondary).
    pub(super) fn is_exact_primary(&self, modifiers: Modifiers) -> bool {
        match self {
            PlatformKeymap::Windows | PlatformKeymap::Linux => {
                modifiers.ctrl && !modifiers.shift && !modifiers.alt && !modifiers.meta
            }
            PlatformKeymap::MacOs => {
                modifiers.meta && !modifiers.shift && !modifiers.alt && !modifiers.ctrl
            }
        }
    }

    /// Word navigation modifier: Ctrl on Win/Linux, Option on macOS.
    pub(super) fn is_word_nav(&self, modifiers: Modifiers) -> bool {
        match self {
            PlatformKeymap::Windows | PlatformKeymap::Linux => {
                modifiers.ctrl && !modifiers.alt && !modifiers.meta
            }
            PlatformKeymap::MacOs => modifiers.alt && !modifiers.ctrl && !modifiers.meta,
        }
    }

    /// macOS Command-arrow navigation, allowing Shift but no secondary modifier.
    pub(super) fn is_command_nav(&self, modifiers: Modifiers) -> bool {
        *self == Self::MacOs && modifiers.meta && !modifiers.ctrl && !modifiers.alt
    }

    /// Local navigation: no primary modifiers.
    pub(super) fn is_local_nav(&self, modifiers: Modifiers) -> bool {
        match self {
            PlatformKeymap::Windows | PlatformKeymap::Linux => {
                !modifiers.ctrl && !modifiers.alt && !modifiers.meta
            }
            PlatformKeymap::MacOs => !modifiers.meta && !modifiers.ctrl && !modifiers.alt,
        }
    }
}
