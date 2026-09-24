use anyhow::Ok;
use ratatui::DefaultTerminal;
use ratatui::Frame;
use ratatui::layout::Constraint;
use ratatui::layout::Flex;
use ratatui::layout::Layout;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Widget;

use crate::model::Model;
use crate::msg::Message;
use crate::msg::get_message;

mod model;
mod msg;

fn main() -> anyhow::Result<()> {
    ratatui::run(run)
}

fn run(terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
    let mut model = Model::new();

    loop {
        terminal.draw(|f| draw(&model, f))?;

        let mut message = get_message()?;

        while let Some(msg) = message {
            if matches!(msg, Message::Quit) {
                return Ok(());
            }

            message = update(&mut model, msg);
        }
    }
}

fn draw(model: &Model, f: &mut Frame) {
    let area = f.area();
    let buf = f.buffer_mut();

    let num_entries = model.paths.len();

    let block = Block::default().borders(Borders::all());
    let inner = block.inner(area);
    block.render(area, buf);

    if num_entries == 0 {
        let center = Layout::vertical([Constraint::Length(1)])
            .flex(Flex::Center)
            .split(inner)[0];

        Line::from("There are no valid entries")
            .centered()
            .render(center, buf);
        return;
    }

    let path = &model.paths[model.path_index];
    let index = model.path_index + 1;

    let layout = Layout::vertical([
        // Clip title
        Constraint::Fill(1),
        // Clip name input
        Constraint::Fill(1),
        // Clip duration input
        Constraint::Fill(1),
        // Clip folder input
        Constraint::Fill(1),
        // Help messages
        Constraint::Length(1),
    ]);
    let spaces = inner.layout::<5>(&layout);

    let line = Line::from(format!("Path: {}", path.file_name().to_str().unwrap()));
    line.render(spaces[0], buf);

    let help_area = spaces[4];

    let help_layout = Layout::horizontal([Constraint::Fill(2), Constraint::Fill(1)]);
    let help_spaces = help_area.layout::<2>(&help_layout);

    let help_line =
        Line::from("q: quit, <-: previous entry, ->: next entry, o: Open in default app");
    help_line.render(help_spaces[0], buf);

    let help_index = Line::from(format!("Entry: {index}/{num_entries}"));
    help_index.render(help_spaces[1], buf);
}

fn update(model: &mut Model, msg: Message) -> Option<Message> {
    match msg {
        Message::Prev => {
            if model.path_index == 0 {
                model.path_index = model.paths.len() - 1;
            } else {
                model.path_index -= 1;
            }
            None
        }
        Message::Next => {
            if model.path_index + 1 == model.paths.len() {
                model.path_index = 0;
                None
            } else {
                model.path_index += 1;
                None
            }
        }
        Message::Open => {
            let clip_path = model.paths[model.path_index].path();
            let _ = open::that_detached(clip_path);
            None
        }
        _ => None,
    }
}
