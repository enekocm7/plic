use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Debug)]
pub enum Message {
    Prev,
    Next,
    Open,
    Quit,
}

pub fn get_message() -> anyhow::Result<Option<Message>> {
    if event::poll(Duration::from_millis(100))?
        && let Event::Key(key) = event::read()?
        && key.kind == KeyEventKind::Press
    {
        return Ok(key_to_message(key));
    }
    Ok(None)
}

pub fn key_to_message(key: KeyEvent) -> Option<Message> {
    match key {
        KeyEvent {
            code: KeyCode::Left,
            ..
        } => Some(Message::Prev),
        KeyEvent {
            code: KeyCode::Right,
            ..
        } => Some(Message::Next),
        KeyEvent {
            code: KeyCode::Char('o'),
            ..
        } => Some(Message::Open),
        KeyEvent {
            code: KeyCode::Char('q'),
            ..
        }
        | KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Some(Message::Quit),
        _ => None,
    }
}
