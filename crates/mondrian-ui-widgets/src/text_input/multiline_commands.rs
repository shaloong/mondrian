use mondrian_ui_core::types::{KeyCode, Modifiers};

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

    /// The primary modifier used for clipboard and document-level shortcuts.
    fn is_primary(&self, modifiers: Modifiers) -> bool {
        match self {
            PlatformKeymap::Windows | PlatformKeymap::Linux => modifiers.ctrl,
            PlatformKeymap::MacOs => modifiers.meta,
        }
    }

    /// Whether modifiers represent "exact primary" (primary only, no alt/shift/secondary).
    fn is_exact_primary(&self, modifiers: Modifiers) -> bool {
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
    fn is_word_nav(&self, modifiers: Modifiers) -> bool {
        match self {
            PlatformKeymap::Windows | PlatformKeymap::Linux => {
                modifiers.ctrl && !modifiers.alt && !modifiers.meta
            }
            PlatformKeymap::MacOs => modifiers.alt && !modifiers.ctrl && !modifiers.meta,
        }
    }

    /// Local navigation: no primary modifiers.
    fn is_local_nav(&self, modifiers: Modifiers) -> bool {
        match self {
            PlatformKeymap::Windows | PlatformKeymap::Linux => {
                !modifiers.ctrl && !modifiers.alt && !modifiers.meta
            }
            PlatformKeymap::MacOs => !modifiers.meta && !modifiers.ctrl && !modifiers.alt,
        }
    }
}

// ── Tab Behavior ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabBehavior {
    /// Tab/Shift+Tab are ignored by the text input, allowing focus traversal.
    MoveFocus,
    /// Tab inserts a `\t` character.
    InsertTab,
    /// Tab indents selected lines (or inserts `\t` when no selection).
    /// Shift+Tab dedents selected lines (unindents).
    IndentSelection,
}

// ── Commands ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MultilineTextKeyCommand {
    // Clipboard
    SelectAll,
    Copy,
    Paste,
    Cut,
    // Horizontal movement
    MoveLeft {
        word: bool,
        extend_selection: bool,
    },
    MoveRight {
        word: bool,
        extend_selection: bool,
    },
    MoveLineStart {
        extend_selection: bool,
    },
    MoveLineEnd {
        extend_selection: bool,
    },
    // Vertical movement
    MoveUp {
        extend_selection: bool,
    },
    MoveDown {
        extend_selection: bool,
    },
    // Document-level
    MoveDocStart {
        extend_selection: bool,
    },
    MoveDocEnd {
        extend_selection: bool,
    },
    // Page-level
    PageUp {
        extend_selection: bool,
    },
    PageDown {
        extend_selection: bool,
    },
    // Delete
    DeleteBackward,
    DeleteForward,
    // Newline
    NewLine,
    // Undo/Redo (text-local)
    Undo,
    Redo,
    // Tab
    InsertTab,
    /// Indent selected lines (Tab with IndentSelection behavior).
    IndentLines,
    /// Dedent selected lines (Shift+Tab with IndentSelection behavior).
    DedentLines,
}

pub(super) fn classify_key_command(
    key: KeyCode,
    modifiers: Modifiers,
    keymap: PlatformKeymap,
    tab_behavior: TabBehavior,
) -> Option<MultilineTextKeyCommand> {
    let shift = modifiers.shift;

    match key {
        // ── Clipboard (exact primary only) ──
        KeyCode::A if keymap.is_exact_primary(modifiers) => {
            Some(MultilineTextKeyCommand::SelectAll)
        }
        KeyCode::C if keymap.is_exact_primary(modifiers) => Some(MultilineTextKeyCommand::Copy),
        KeyCode::V if keymap.is_exact_primary(modifiers) => Some(MultilineTextKeyCommand::Paste),
        KeyCode::X if keymap.is_exact_primary(modifiers) => Some(MultilineTextKeyCommand::Cut),

        // ── Undo/Redo (primary modifier) ──
        KeyCode::Z if keymap.is_primary(modifiers) && !modifiers.shift && !modifiers.alt => {
            Some(MultilineTextKeyCommand::Undo)
        }
        KeyCode::Z if keymap.is_primary(modifiers) && modifiers.shift && !modifiers.alt => {
            Some(MultilineTextKeyCommand::Redo)
        }

        // ── Left/Right: word nav on word_modifier, local otherwise ──
        KeyCode::Left if keymap.is_word_nav(modifiers) => {
            Some(MultilineTextKeyCommand::MoveLeft { word: true, extend_selection: shift })
        }
        KeyCode::Right if keymap.is_word_nav(modifiers) => {
            Some(MultilineTextKeyCommand::MoveRight { word: true, extend_selection: shift })
        }
        KeyCode::Left if keymap.is_local_nav(modifiers) => {
            Some(MultilineTextKeyCommand::MoveLeft { word: false, extend_selection: shift })
        }
        KeyCode::Right if keymap.is_local_nav(modifiers) => {
            Some(MultilineTextKeyCommand::MoveRight { word: false, extend_selection: shift })
        }

        // ── Home/End: document-level with primary+no_shift, line-level with local ──
        KeyCode::Home if keymap.is_exact_primary(modifiers) => {
            Some(MultilineTextKeyCommand::MoveDocStart { extend_selection: false })
        }
        KeyCode::End if keymap.is_exact_primary(modifiers) => {
            Some(MultilineTextKeyCommand::MoveDocEnd { extend_selection: false })
        }
        KeyCode::Home if keymap.is_local_nav(modifiers) => {
            Some(MultilineTextKeyCommand::MoveLineStart { extend_selection: shift })
        }
        KeyCode::End if keymap.is_local_nav(modifiers) => {
            Some(MultilineTextKeyCommand::MoveLineEnd { extend_selection: shift })
        }
        // Also accept word_nav for Home/End (macOS Ctrl+A/Ctrl+E style)
        KeyCode::Home if keymap.is_word_nav(modifiers) && cfg!(not(target_os = "macos")) => {
            Some(MultilineTextKeyCommand::MoveLineStart { extend_selection: shift })
        }
        KeyCode::End if keymap.is_word_nav(modifiers) && cfg!(not(target_os = "macos")) => {
            Some(MultilineTextKeyCommand::MoveLineEnd { extend_selection: shift })
        }

        // ── Up/Down (local only) ──
        KeyCode::Up if keymap.is_local_nav(modifiers) => {
            Some(MultilineTextKeyCommand::MoveUp { extend_selection: shift })
        }
        KeyCode::Down if keymap.is_local_nav(modifiers) => {
            Some(MultilineTextKeyCommand::MoveDown { extend_selection: shift })
        }

        // ── PageUp/PageDown (local only) ──
        KeyCode::PageUp if keymap.is_local_nav(modifiers) => {
            Some(MultilineTextKeyCommand::PageUp { extend_selection: shift })
        }
        KeyCode::PageDown if keymap.is_local_nav(modifiers) => {
            Some(MultilineTextKeyCommand::PageDown { extend_selection: shift })
        }

        // ── Delete (plain, no modifiers) ──
        KeyCode::Backspace if modifiers == Modifiers::none() => {
            Some(MultilineTextKeyCommand::DeleteBackward)
        }
        KeyCode::Delete if modifiers == Modifiers::none() => {
            Some(MultilineTextKeyCommand::DeleteForward)
        }

        // ── Enter (plain) ──
        KeyCode::Enter if modifiers == Modifiers::none() => Some(MultilineTextKeyCommand::NewLine),

        // ── Tab: depends on TabBehavior ──
        KeyCode::Tab
            if modifiers == Modifiers::none() && tab_behavior == TabBehavior::InsertTab =>
        {
            Some(MultilineTextKeyCommand::InsertTab)
        }
        KeyCode::Tab
            if modifiers == Modifiers::none() && tab_behavior == TabBehavior::IndentSelection =>
        {
            Some(MultilineTextKeyCommand::IndentLines)
        }
        KeyCode::Tab
            if modifiers.shift
                && !modifiers.ctrl
                && !modifiers.alt
                && !modifiers.meta
                && tab_behavior == TabBehavior::IndentSelection =>
        {
            Some(MultilineTextKeyCommand::DedentLines)
        }
        // Shift+Tab with MoveFocus or InsertTab is always passed through
        KeyCode::Tab if modifiers.shift && !modifiers.ctrl && !modifiers.alt && !modifiers.meta => {
            None
        }

        _ => None,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn win() -> PlatformKeymap {
        PlatformKeymap::Windows
    }
    fn mac() -> PlatformKeymap {
        PlatformKeymap::MacOs
    }
    fn mb() -> TabBehavior {
        TabBehavior::MoveFocus
    }
    fn _itab() -> TabBehavior {
        TabBehavior::InsertTab
    }

    #[test]
    fn exact_primary_clipboard_win() {
        let km = win();
        assert_eq!(
            classify_key_command(KeyCode::A, Modifiers::ctrl(), km, mb()),
            Some(MultilineTextKeyCommand::SelectAll)
        );
        assert_eq!(
            classify_key_command(KeyCode::V, Modifiers::ctrl(), km, mb()),
            Some(MultilineTextKeyCommand::Paste)
        );
    }

    #[test]
    fn exact_primary_clipboard_mac() {
        let km = mac();
        assert_eq!(
            classify_key_command(
                KeyCode::A,
                Modifiers { meta: true, ..Modifiers::none() },
                km,
                mb()
            ),
            Some(MultilineTextKeyCommand::SelectAll)
        );
        assert_eq!(
            classify_key_command(
                KeyCode::V,
                Modifiers { meta: true, ..Modifiers::none() },
                km,
                mb()
            ),
            Some(MultilineTextKeyCommand::Paste)
        );
    }

    #[test]
    fn modified_clipboard_chords_pass_through() {
        let km = win();
        let ctrl_shift = Modifiers { ctrl: true, shift: true, ..Modifiers::none() };
        assert_eq!(classify_key_command(KeyCode::A, ctrl_shift, km, mb()), None);
        assert_eq!(
            classify_key_command(
                KeyCode::V,
                Modifiers { ctrl: true, alt: true, ..Modifiers::none() },
                km,
                mb()
            ),
            None
        );
    }

    #[test]
    fn undo_redo_captured() {
        let km = win();
        assert_eq!(
            classify_key_command(KeyCode::Z, Modifiers::ctrl(), km, mb()),
            Some(MultilineTextKeyCommand::Undo)
        );
        assert_eq!(
            classify_key_command(
                KeyCode::Z,
                Modifiers { ctrl: true, shift: true, ..Modifiers::none() },
                km,
                mb()
            ),
            Some(MultilineTextKeyCommand::Redo)
        );
    }

    #[test]
    fn navigation_with_and_without_selection() {
        let km = win();
        assert_eq!(
            classify_key_command(KeyCode::Left, Modifiers::none(), km, mb()),
            Some(MultilineTextKeyCommand::MoveLeft { word: false, extend_selection: false })
        );
        assert_eq!(
            classify_key_command(KeyCode::Right, Modifiers::shift(), km, mb()),
            Some(MultilineTextKeyCommand::MoveRight { word: false, extend_selection: true })
        );
        assert_eq!(
            classify_key_command(KeyCode::Left, Modifiers::ctrl(), km, mb()),
            Some(MultilineTextKeyCommand::MoveLeft { word: true, extend_selection: false })
        );
    }

    #[test]
    fn mac_word_nav_uses_option() {
        let km = mac();
        assert_eq!(
            classify_key_command(
                KeyCode::Left,
                Modifiers { alt: true, ..Modifiers::none() },
                km,
                mb()
            ),
            Some(MultilineTextKeyCommand::MoveLeft { word: true, extend_selection: false })
        );
    }

    #[test]
    fn home_end_doc_vs_line() {
        let km = win();
        assert_eq!(
            classify_key_command(KeyCode::Home, Modifiers::ctrl(), km, mb()),
            Some(MultilineTextKeyCommand::MoveDocStart { extend_selection: false })
        );
        assert_eq!(
            classify_key_command(KeyCode::End, Modifiers::ctrl(), km, mb()),
            Some(MultilineTextKeyCommand::MoveDocEnd { extend_selection: false })
        );
        assert_eq!(
            classify_key_command(KeyCode::Home, Modifiers::none(), km, mb()),
            Some(MultilineTextKeyCommand::MoveLineStart { extend_selection: false })
        );
        assert_eq!(
            classify_key_command(KeyCode::End, Modifiers::shift(), km, mb()),
            Some(MultilineTextKeyCommand::MoveLineEnd { extend_selection: true })
        );
    }

    #[test]
    fn up_down_page() {
        let km = win();
        assert_eq!(
            classify_key_command(KeyCode::Up, Modifiers::none(), km, mb()),
            Some(MultilineTextKeyCommand::MoveUp { extend_selection: false })
        );
        assert_eq!(
            classify_key_command(KeyCode::Down, Modifiers::shift(), km, mb()),
            Some(MultilineTextKeyCommand::MoveDown { extend_selection: true })
        );
        assert_eq!(
            classify_key_command(KeyCode::PageUp, Modifiers::none(), km, mb()),
            Some(MultilineTextKeyCommand::PageUp { extend_selection: false })
        );
        assert_eq!(
            classify_key_command(KeyCode::PageDown, Modifiers::shift(), km, mb()),
            Some(MultilineTextKeyCommand::PageDown { extend_selection: true })
        );
    }

    #[test]
    fn enter_and_tab() {
        let km = win();
        assert_eq!(
            classify_key_command(KeyCode::Enter, Modifiers::none(), km, mb()),
            Some(MultilineTextKeyCommand::NewLine)
        );
        // Tab with MoveFocus → None (not consumed by text input)
        assert_eq!(
            classify_key_command(KeyCode::Tab, Modifiers::none(), km, mb()),
            None
        );
    }

    #[test]
    fn alt_and_meta_chords_pass_through() {
        let km = win();
        assert_eq!(
            classify_key_command(
                KeyCode::Up,
                Modifiers { alt: true, ..Modifiers::none() },
                km,
                mb()
            ),
            None
        );
        assert_eq!(
            classify_key_command(
                KeyCode::Down,
                Modifiers { meta: true, ..Modifiers::none() },
                km,
                mb()
            ),
            None
        );
    }

    #[test]
    fn delete_requires_no_modifiers() {
        let km = win();
        assert_eq!(
            classify_key_command(KeyCode::Backspace, Modifiers::none(), km, mb()),
            Some(MultilineTextKeyCommand::DeleteBackward)
        );
        assert_eq!(
            classify_key_command(KeyCode::Delete, Modifiers::none(), km, mb()),
            Some(MultilineTextKeyCommand::DeleteForward)
        );
        assert_eq!(
            classify_key_command(KeyCode::Backspace, Modifiers::ctrl(), km, mb()),
            None
        );
    }

    // ── IndentSelection tests ───────────────────────────────────────────────

    fn _ind() -> TabBehavior {
        TabBehavior::IndentSelection
    }

    #[test]
    fn indent_selection_tab_classified() {
        let km = win();
        assert_eq!(
            classify_key_command(KeyCode::Tab, Modifiers::none(), km, _ind()),
            Some(MultilineTextKeyCommand::IndentLines)
        );
    }

    #[test]
    fn indent_selection_shift_tab_classified_as_dedent() {
        let km = win();
        assert_eq!(
            classify_key_command(KeyCode::Tab, Modifiers::shift(), km, _ind()),
            Some(MultilineTextKeyCommand::DedentLines)
        );
    }

    #[test]
    fn indent_selection_shift_tab_with_move_focus_passes_through() {
        let km = win();
        assert_eq!(
            classify_key_command(KeyCode::Tab, Modifiers::shift(), km, mb()),
            None
        );
    }
}
