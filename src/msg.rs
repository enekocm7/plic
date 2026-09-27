use std::time::Duration;

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};

use crate::model::{EditField, ExportResult};

#[derive(Debug)]
pub enum Message {
    PreviousClip,
    NextClip,
    Open,
    BeginEdit,
    CancelEdit,
    FocusNext,
    FocusPrevious,
    FolderPrevious,
    FolderNext,
    CursorLeft,
    CursorRight,
    Backspace,
    ClearField,
    Insert(char),
    Export,
    ExportFinished(ExportResult),
    Quit,
}

pub fn get_message(editing: bool, field: EditField) -> anyhow::Result<Option<Message>> {
    if event::poll(Duration::from_millis(100))?
        && let Event::Key(key) = event::read()?
        && key.kind == KeyEventKind::Press
    {
        return Ok(key_to_message(key, editing, field));
    }
    Ok(None)
}

fn key_to_message(key: KeyEvent, editing: bool, field: EditField) -> Option<Message> {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(Message::Quit);
    }

    if editing {
        if key.code == KeyCode::Char('a')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && field != EditField::Folder
        {
            return Some(Message::ClearField);
        }
        return match key.code {
            KeyCode::Esc => Some(Message::CancelEdit),
            KeyCode::Enter => Some(Message::Export),
            KeyCode::BackTab => Some(Message::FocusPrevious),
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Some(Message::FocusPrevious)
            }
            KeyCode::Tab => Some(Message::FocusNext),
            KeyCode::Up if field == EditField::Folder => Some(Message::FolderPrevious),
            KeyCode::Down if field == EditField::Folder => Some(Message::FolderNext),
            KeyCode::Left if field != EditField::Folder => Some(Message::CursorLeft),
            KeyCode::Right if field != EditField::Folder => Some(Message::CursorRight),
            KeyCode::Backspace if field != EditField::Folder => Some(Message::Backspace),
            KeyCode::Char(character)
                if field != EditField::Folder
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                Some(Message::Insert(character))
            }
            _ => None,
        };
    }

    match key.code {
        KeyCode::Left => Some(Message::PreviousClip),
        KeyCode::Right => Some(Message::NextClip),
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'o') => Some(Message::Open),
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'e') => {
            Some(Message::BeginEdit)
        }
        KeyCode::Char(character) if character.eq_ignore_ascii_case(&'q') => Some(Message::Quit),
        _ => None,
    }
}
