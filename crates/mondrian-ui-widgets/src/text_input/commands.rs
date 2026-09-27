use mondrian_ui_core::types::{KeyCode, Modifiers};

use super::multiline_commands::PlatformKeymap;

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
}

pub(super) fn classify_key_command(
    key: KeyCode,
    modifiers: Modifiers,
    keymap: PlatformKeymap,
) -> Option<TextInputKeyCommand> {
    let shift = modifiers.shift;
    let plain_delete = modifiers == Modifiers::none();

    match key {
        KeyCode::A if keymap.is_exact_primary(modifiers) => Some(TextInputKeyCommand::SelectAll),
        KeyCode::C if keymap.is_exact_primary(modifiers) => Some(TextInputKeyCommand::Copy),
        KeyCode::V if keymap.is_exact_primary(modifiers) => Some(TextInputKeyCommand::Paste),
        KeyCode::X if keymap.is_exact_primary(modifiers) => Some(TextInputKeyCommand::Cut),
        KeyCode::Left | KeyCode::Up if keymap.is_mac_primary_nav(modifiers) => {
            Some(TextInputKeyCommand::MoveHome { extend_selection: shift })
        }
        KeyCode::Right | KeyCode::Down if keymap.is_mac_primary_nav(modifiers) => {
            Some(TextInputKeyCommand::MoveEnd { extend_selection: shift })
        }
        KeyCode::Left if keymap.is_word_nav(modifiers) => {
            Some(TextInputKeyCommand::MoveLeft { word: true, extend_selection: shift })
        }
        KeyCode::Right if keymap.is_word_nav(modifiers) => {
            Some(TextInputKeyCommand::MoveRight { word: true, extend_selection: shift })
        }
        KeyCode::Home if keymap.is_word_nav(modifiers) => {
            Some(TextInputKeyCommand::MoveHome { extend_selection: shift })
        }
        KeyCode::End if keymap.is_word_nav(modifiers) => {
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

    #[test]
    fn modified_text_shortcuts_are_left_for_global_routing() {
        for keymap in [
            PlatformKeymap::Windows,
            PlatformKeymap::Linux,
            PlatformKeymap::MacOs,
        ] {
            assert_eq!(
                classify_key_command(
                    KeyCode::A,
                    Modifiers { ctrl: true, shift: true, ..Modifiers::none() },
                    keymap
                ),
                None
            );
            assert_eq!(
                classify_key_command(
                    KeyCode::V,
                    Modifiers { ctrl: true, alt: true, ..Modifiers::none() },
                    keymap
                ),
                None
            );
            assert_eq!(
                classify_key_command(
                    KeyCode::X,
                    Modifiers { ctrl: true, meta: true, ..Modifiers::none() },
                    keymap
                ),
                None
            );
        }
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
            classify_key_command(
                KeyCode::Right,
                Modifiers { ctrl: true, shift: true, ..Modifiers::none() },
                PlatformKeymap::Windows
            ),
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
}
