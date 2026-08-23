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
}

pub(super) fn classify_key_command(
    key: KeyCode,
    modifiers: Modifiers,
) -> Option<TextInputKeyCommand> {
    let shift = modifiers.shift;
    let exact_ctrl = modifiers.ctrl && !modifiers.shift && !modifiers.alt && !modifiers.meta;
    let word_navigation = modifiers.ctrl && !modifiers.alt && !modifiers.meta;
    let local_navigation = !modifiers.ctrl && !modifiers.alt && !modifiers.meta;
    let plain_delete = modifiers == Modifiers::none();

    match key {
        KeyCode::A if exact_ctrl => Some(TextInputKeyCommand::SelectAll),
        KeyCode::C if exact_ctrl => Some(TextInputKeyCommand::Copy),
        KeyCode::V if exact_ctrl => Some(TextInputKeyCommand::Paste),
        KeyCode::X if exact_ctrl => Some(TextInputKeyCommand::Cut),
        KeyCode::Left if word_navigation => {
            Some(TextInputKeyCommand::MoveLeft { word: true, extend_selection: shift })
        }
        KeyCode::Right if word_navigation => {
            Some(TextInputKeyCommand::MoveRight { word: true, extend_selection: shift })
        }
        KeyCode::Home if word_navigation => {
            Some(TextInputKeyCommand::MoveHome { extend_selection: shift })
        }
        KeyCode::End if word_navigation => {
            Some(TextInputKeyCommand::MoveEnd { extend_selection: shift })
        }
        KeyCode::Backspace if plain_delete => Some(TextInputKeyCommand::DeleteBackward),
        KeyCode::Delete if plain_delete => Some(TextInputKeyCommand::DeleteForward),
        KeyCode::Left if local_navigation => {
            Some(TextInputKeyCommand::MoveLeft { word: false, extend_selection: shift })
        }
        KeyCode::Right if local_navigation => {
            Some(TextInputKeyCommand::MoveRight { word: false, extend_selection: shift })
        }
        KeyCode::Home if local_navigation => {
            Some(TextInputKeyCommand::MoveHome { extend_selection: shift })
        }
        KeyCode::End if local_navigation => {
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
    fn exact_ctrl_shortcuts_are_local_text_commands() {
        assert_eq!(
            classify_key_command(KeyCode::A, Modifiers::ctrl()),
            Some(TextInputKeyCommand::SelectAll)
        );
        assert_eq!(
            classify_key_command(KeyCode::C, Modifiers::ctrl()),
            Some(TextInputKeyCommand::Copy)
        );
        assert_eq!(
            classify_key_command(KeyCode::V, Modifiers::ctrl()),
            Some(TextInputKeyCommand::Paste)
        );
        assert_eq!(
            classify_key_command(KeyCode::X, Modifiers::ctrl()),
            Some(TextInputKeyCommand::Cut)
        );
    }

    #[test]
    fn modified_text_shortcuts_are_left_for_global_routing() {
        assert_eq!(classify_key_command(KeyCode::A, ctrl_shift()), None);
        assert_eq!(classify_key_command(KeyCode::V, ctrl_alt()), None);
        assert_eq!(
            classify_key_command(
                KeyCode::X,
                Modifiers { ctrl: true, meta: true, ..Modifiers::none() }
            ),
            None
        );
    }

    #[test]
    fn navigation_preserves_word_and_selection_intent() {
        assert_eq!(
            classify_key_command(KeyCode::Left, Modifiers::none()),
            Some(TextInputKeyCommand::MoveLeft { word: false, extend_selection: false })
        );
        assert_eq!(
            classify_key_command(KeyCode::Right, Modifiers::shift()),
            Some(TextInputKeyCommand::MoveRight { word: false, extend_selection: true })
        );
        assert_eq!(
            classify_key_command(KeyCode::Left, Modifiers::ctrl()),
            Some(TextInputKeyCommand::MoveLeft { word: true, extend_selection: false })
        );
        assert_eq!(
            classify_key_command(KeyCode::Right, ctrl_shift()),
            Some(TextInputKeyCommand::MoveRight { word: true, extend_selection: true })
        );
    }

    #[test]
    fn alt_and_meta_navigation_are_left_for_shortcuts_or_system() {
        assert_eq!(
            classify_key_command(KeyCode::Left, Modifiers { alt: true, ..Modifiers::none() }),
            None
        );
        assert_eq!(
            classify_key_command(
                KeyCode::Right,
                Modifiers { meta: true, ..Modifiers::none() }
            ),
            None
        );
    }

    #[test]
    fn delete_commands_require_no_modifiers() {
        assert_eq!(
            classify_key_command(KeyCode::Backspace, Modifiers::none()),
            Some(TextInputKeyCommand::DeleteBackward)
        );
        assert_eq!(
            classify_key_command(KeyCode::Delete, Modifiers::none()),
            Some(TextInputKeyCommand::DeleteForward)
        );
        assert_eq!(
            classify_key_command(KeyCode::Backspace, Modifiers::ctrl()),
            None
        );
        assert_eq!(
            classify_key_command(KeyCode::Delete, Modifiers::shift()),
            None
        );
    }
}
