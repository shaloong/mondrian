use super::keymap::PlatformKeymap;
use mondrian_ui_core::types::{KeyCode, Modifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TextInputKeyCommand {
    SelectAll,
    Copy,
    Paste,
    Cut,
    MoveLeft { word: bool, extend_selection: bool },
    MoveRight { word: bool, extend_selection: bool },
    MoveHome { extend_selection: bool },
    MoveEnd { extend_selection: bool },
    DeleteBackward,
    DeleteForward,
    DeleteWordBackward,
    DeleteWordForward,
    Undo,
    Redo,
}

pub(super) fn classify_key_command(
    key: KeyCode,
    modifiers: Modifiers,
    keymap: PlatformKeymap,
) -> Option<TextInputKeyCommand> {
    let shift = modifiers.shift;
    let exact_primary = keymap.is_exact_primary(modifiers);
    let primary_history = keymap.is_exact_primary(Modifiers { shift: false, ..modifiers });
    let word_navigation = keymap.is_word_nav(modifiers);
    let command_navigation = keymap.is_command_nav(modifiers);
    let plain_delete = modifiers == Modifiers::none();

    match key {
        KeyCode::Z if primary_history && !shift => Some(TextInputKeyCommand::Undo),
        KeyCode::Z if primary_history && shift => Some(TextInputKeyCommand::Redo),
        KeyCode::Backspace if word_navigation && !shift => {
            Some(TextInputKeyCommand::DeleteWordBackward)
        }
        KeyCode::Delete if word_navigation && !shift => {
            Some(TextInputKeyCommand::DeleteWordForward)
        }
        KeyCode::A if exact_primary => Some(TextInputKeyCommand::SelectAll),
        KeyCode::C if exact_primary => Some(TextInputKeyCommand::Copy),
        KeyCode::V if exact_primary => Some(TextInputKeyCommand::Paste),
        KeyCode::X if exact_primary => Some(TextInputKeyCommand::Cut),
        KeyCode::Left | KeyCode::Up if command_navigation => {
            Some(TextInputKeyCommand::MoveHome { extend_selection: shift })
        }
        KeyCode::Right | KeyCode::Down if command_navigation => {
            Some(TextInputKeyCommand::MoveEnd { extend_selection: shift })
        }
        KeyCode::Left if word_navigation => {
            Some(TextInputKeyCommand::MoveLeft { word: true, extend_selection: shift })
        }
        KeyCode::Right if word_navigation => {
            Some(TextInputKeyCommand::MoveRight { word: true, extend_selection: shift })
        }
        KeyCode::Home if word_navigation && keymap != PlatformKeymap::MacOs => {
            Some(TextInputKeyCommand::MoveHome { extend_selection: shift })
        }
        KeyCode::End if word_navigation && keymap != PlatformKeymap::MacOs => {
            Some(TextInputKeyCommand::MoveEnd { extend_selection: shift })
        }
        KeyCode::Backspace if plain_delete => Some(TextInputKeyCommand::DeleteBackward),
        KeyCode::Delete if plain_delete => Some(TextInputKeyCommand::DeleteForward),
        KeyCode::Left if keymap.is_local_nav(modifiers) => {
            Some(TextInputKeyCommand::MoveLeft { word: false, extend_selection: shift })
        }
        KeyCode::Right if keymap.is_local_nav(modifiers) => {
            Some(TextInputKeyCommand::MoveRight { word: false, extend_selection: shift })
        }
        KeyCode::Home if keymap.is_local_nav(modifiers) => {
            Some(TextInputKeyCommand::MoveHome { extend_selection: shift })
        }
        KeyCode::End if keymap.is_local_nav(modifiers) => {
            Some(TextInputKeyCommand::MoveEnd { extend_selection: shift })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl_shift() -> Modifiers {
        Modifiers { ctrl: true, shift: true, ..Modifiers::none() }
    }

    fn ctrl_alt() -> Modifiers {
        Modifiers { ctrl: true, alt: true, ..Modifiers::none() }
    }

    #[test]
    fn unrelated_modified_navigation_is_left_for_shortcuts_or_system() {
        assert_eq!(
            classify_key_command(
                KeyCode::Left,
                Modifiers { alt: true, ..Modifiers::none() },
                PlatformKeymap::Windows
            ),
            None
        );
        assert_eq!(
            classify_key_command(
                KeyCode::Right,
                Modifiers { meta: true, alt: true, ..Modifiers::none() },
                PlatformKeymap::MacOs
            ),
            None
        );
    }

    #[test]
    fn delete_commands_require_no_modifiers() {
        assert_eq!(
            classify_key_command(KeyCode::Backspace, Modifiers::none(), PlatformKeymap::MacOs),
            Some(TextInputKeyCommand::DeleteBackward)
        );
        assert_eq!(
            classify_key_command(KeyCode::Delete, Modifiers::none(), PlatformKeymap::MacOs),
            Some(TextInputKeyCommand::DeleteForward)
        );
        assert_eq!(
            classify_key_command(KeyCode::Backspace, Modifiers::ctrl(), PlatformKeymap::MacOs),
            None
        );
        assert_eq!(
            classify_key_command(KeyCode::Delete, Modifiers::shift(), PlatformKeymap::MacOs),
            None
        );
    }

    #[test]
    fn exact_ctrl_shortcuts_are_local_text_commands() {
        assert_eq!(
            classify_key_command(KeyCode::A, Modifiers::ctrl(), PlatformKeymap::Windows),
            Some(TextInputKeyCommand::SelectAll)
        );
        assert_eq!(
            classify_key_command(KeyCode::C, Modifiers::ctrl(), PlatformKeymap::Windows),
            Some(TextInputKeyCommand::Copy)
        );
        assert_eq!(
            classify_key_command(KeyCode::V, Modifiers::ctrl(), PlatformKeymap::Windows),
            Some(TextInputKeyCommand::Paste)
        );
        assert_eq!(
            classify_key_command(KeyCode::X, Modifiers::ctrl(), PlatformKeymap::Windows),
            Some(TextInputKeyCommand::Cut)
        );
    }

    #[test]
    fn modified_text_shortcuts_are_left_for_global_routing() {
        assert_eq!(
            classify_key_command(KeyCode::A, ctrl_shift(), PlatformKeymap::Windows),
            None
        );
        assert_eq!(
            classify_key_command(KeyCode::V, ctrl_alt(), PlatformKeymap::Windows),
            None
        );
        assert_eq!(
            classify_key_command(
                KeyCode::X,
                Modifiers { ctrl: true, meta: true, ..Modifiers::none() },
                PlatformKeymap::Windows
            ),
            None
        );
    }

    #[test]
    fn navigation_preserves_word_and_selection_intent() {
        assert_eq!(
            classify_key_command(KeyCode::Left, Modifiers::none(), PlatformKeymap::Windows),
            Some(TextInputKeyCommand::MoveLeft { word: false, extend_selection: false })
        );
        assert_eq!(
            classify_key_command(KeyCode::Right, Modifiers::shift(), PlatformKeymap::Windows),
            Some(TextInputKeyCommand::MoveRight { word: false, extend_selection: true })
        );
        assert_eq!(
            classify_key_command(KeyCode::Left, Modifiers::ctrl(), PlatformKeymap::Windows),
            Some(TextInputKeyCommand::MoveLeft { word: true, extend_selection: false })
        );
        assert_eq!(
            classify_key_command(KeyCode::Right, ctrl_shift(), PlatformKeymap::Windows),
            Some(TextInputKeyCommand::MoveRight { word: true, extend_selection: true })
        );
        assert_eq!(
            classify_key_command(
                KeyCode::Left,
                Modifiers { alt: true, ..Modifiers::none() },
                PlatformKeymap::MacOs
            ),
            Some(TextInputKeyCommand::MoveLeft { word: true, extend_selection: false })
        );
    }

    #[test]
    fn mac_command_arrows_reach_single_line_edges_and_extend_selection() {
        let command = Modifiers { meta: true, ..Modifiers::none() };
        let command_shift = Modifiers { meta: true, shift: true, ..Modifiers::none() };
        for key in [KeyCode::Left, KeyCode::Up] {
            assert_eq!(
                classify_key_command(key, command, PlatformKeymap::MacOs),
                Some(TextInputKeyCommand::MoveHome { extend_selection: false })
            );
            assert_eq!(
                classify_key_command(key, command_shift, PlatformKeymap::MacOs),
                Some(TextInputKeyCommand::MoveHome { extend_selection: true })
            );
            assert_eq!(
                classify_key_command(key, command, PlatformKeymap::Windows),
                None
            );
        }
        for key in [KeyCode::Right, KeyCode::Down] {
            assert_eq!(
                classify_key_command(key, command, PlatformKeymap::MacOs),
                Some(TextInputKeyCommand::MoveEnd { extend_selection: false })
            );
            assert_eq!(
                classify_key_command(key, command_shift, PlatformKeymap::MacOs),
                Some(TextInputKeyCommand::MoveEnd { extend_selection: true })
            );
            assert_eq!(
                classify_key_command(key, command, PlatformKeymap::Linux),
                None
            );
        }
    }

    #[test]
    fn alt_and_meta_navigation_are_left_for_shortcuts_or_system() {
        assert_eq!(
            classify_key_command(
                KeyCode::Left,
                Modifiers { alt: true, ..Modifiers::none() },
                PlatformKeymap::Windows
            ),
            None
        );
        assert_eq!(
            classify_key_command(
                KeyCode::Right,
                Modifiers { meta: true, ..Modifiers::none() },
                PlatformKeymap::Windows
            ),
            None
        );
    }

    #[test]
    fn delete_commands_reject_conflicting_modifiers() {
        assert_eq!(
            classify_key_command(
                KeyCode::Backspace,
                Modifiers::none(),
                PlatformKeymap::Windows
            ),
            Some(TextInputKeyCommand::DeleteBackward)
        );
        assert_eq!(
            classify_key_command(KeyCode::Delete, Modifiers::none(), PlatformKeymap::Windows),
            Some(TextInputKeyCommand::DeleteForward)
        );
        assert_eq!(
            classify_key_command(
                KeyCode::Backspace,
                Modifiers { ctrl: true, alt: true, ..Modifiers::none() },
                PlatformKeymap::Windows
            ),
            None
        );
        assert_eq!(
            classify_key_command(KeyCode::Delete, Modifiers::shift(), PlatformKeymap::Windows),
            None
        );
    }

    #[test]
    fn macos_single_line_primary_clipboard_chords_are_owned_text_commands() {
        let command = Modifiers { meta: true, ..Modifiers::none() };
        for (key, expected) in [
            (KeyCode::A, TextInputKeyCommand::SelectAll),
            (KeyCode::C, TextInputKeyCommand::Copy),
            (KeyCode::X, TextInputKeyCommand::Cut),
            (KeyCode::V, TextInputKeyCommand::Paste),
        ] {
            assert_eq!(
                classify_key_command(key, command, PlatformKeymap::MacOs),
                Some(expected)
            );
            assert_eq!(
                classify_key_command(
                    key,
                    Modifiers { shift: true, ..command },
                    PlatformKeymap::MacOs
                ),
                None
            );
            assert_eq!(
                classify_key_command(
                    key,
                    Modifiers { alt: true, ..command },
                    PlatformKeymap::MacOs
                ),
                None
            );
            assert_eq!(
                classify_key_command(
                    key,
                    Modifiers { ctrl: true, ..command },
                    PlatformKeymap::MacOs
                ),
                None
            );
        }
    }

    #[test]
    fn macos_single_line_option_arrows_preserve_word_and_selection_intent() {
        let option = Modifiers { alt: true, ..Modifiers::none() };
        for shift in [false, true] {
            let modifiers = Modifiers { shift, ..option };
            assert_eq!(
                classify_key_command(KeyCode::Left, modifiers, PlatformKeymap::MacOs),
                Some(TextInputKeyCommand::MoveLeft { word: true, extend_selection: shift })
            );
            assert_eq!(
                classify_key_command(KeyCode::Right, modifiers, PlatformKeymap::MacOs),
                Some(TextInputKeyCommand::MoveRight { word: true, extend_selection: shift })
            );
        }
    }

    #[test]
    fn macos_single_line_command_arrows_preserve_line_and_selection_intent() {
        let command = Modifiers { meta: true, ..Modifiers::none() };
        for shift in [false, true] {
            let modifiers = Modifiers { shift, ..command };
            assert_eq!(
                classify_key_command(KeyCode::Left, modifiers, PlatformKeymap::MacOs),
                Some(TextInputKeyCommand::MoveHome { extend_selection: shift })
            );
            assert_eq!(
                classify_key_command(KeyCode::Right, modifiers, PlatformKeymap::MacOs),
                Some(TextInputKeyCommand::MoveEnd { extend_selection: shift })
            );
        }
    }

    #[test]
    fn portable_single_line_history_chords_admit_exact_primary_and_shift_only() {
        for keymap in [
            PlatformKeymap::Windows,
            PlatformKeymap::Linux,
            PlatformKeymap::MacOs,
        ] {
            let primary = if keymap == PlatformKeymap::MacOs {
                Modifiers { meta: true, ..Modifiers::none() }
            } else {
                Modifiers::ctrl()
            };
            for shift in [false, true] {
                assert!(
                    classify_key_command(KeyCode::Z, Modifiers { shift, ..primary }, keymap)
                        .is_some(),
                    "{keymap:?} primary history owns both Undo and Redo"
                );
            }
            for modifiers in [
                Modifiers { alt: true, ..primary },
                Modifiers { ctrl: true, meta: true, ..Modifiers::none() },
            ] {
                assert!(classify_key_command(KeyCode::Z, modifiers, keymap).is_none());
            }
        }
    }
    #[test]
    fn portable_single_line_word_delete_admits_control_or_option_without_secondary_or_shift() {
        for keymap in [
            PlatformKeymap::Windows,
            PlatformKeymap::Linux,
            PlatformKeymap::MacOs,
        ] {
            let word = if keymap == PlatformKeymap::MacOs {
                Modifiers { alt: true, ..Modifiers::none() }
            } else {
                Modifiers::ctrl()
            };
            for key in [KeyCode::Backspace, KeyCode::Delete] {
                assert!(
                    classify_key_command(key, word, keymap).is_some(),
                    "{keymap:?} word deletion"
                );
                assert!(
                    classify_key_command(key, Modifiers { shift: true, ..word }, keymap).is_none()
                );
                assert!(
                    classify_key_command(key, Modifiers { meta: true, ..word }, keymap).is_none()
                );
            }
        }
    }

    #[test]
    fn clipboard_shortcuts_use_the_platform_primary_modifier() {
        for (keymap, primary, secondary) in [
            (
                PlatformKeymap::Windows,
                Modifiers::ctrl(),
                Modifiers { meta: true, ..Modifiers::none() },
            ),
            (
                PlatformKeymap::Linux,
                Modifiers::ctrl(),
                Modifiers { meta: true, ..Modifiers::none() },
            ),
            (
                PlatformKeymap::MacOs,
                Modifiers { meta: true, ..Modifiers::none() },
                Modifiers::ctrl(),
            ),
        ] {
            for (key, expected) in [
                (KeyCode::A, TextInputKeyCommand::SelectAll),
                (KeyCode::C, TextInputKeyCommand::Copy),
                (KeyCode::V, TextInputKeyCommand::Paste),
                (KeyCode::X, TextInputKeyCommand::Cut),
            ] {
                assert_eq!(classify_key_command(key, primary, keymap), Some(expected));
                assert_eq!(classify_key_command(key, secondary, keymap), None);
            }
        }
    }
}
