use std::path::Path;

use anyhow::Result;
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};

use crate::model::{EditField, Model, format_timestamp, parse_timestamp};
use crate::msg::Message;
use crate::msg::get_message;
use crate::text_input::TextInput;

mod ffmpeg;
mod model;
mod msg;
mod text_input;

fn main() -> Result<()> {
    ratatui::run(run)
}

fn run(terminal: &mut DefaultTerminal) -> Result<()> {
    let mut model = Model::new()?;

    loop {
        terminal.draw(|f| draw(&model, f))?;

        if let Some(result) = model.try_export_result() {
            update(&mut model, Message::ExportFinished(result));
            continue;
        }

        if let Some(message) = get_message(model.editing, model.field)?
            && !update(&mut model, message)
        {
            return Ok(());
        }
    }
}

fn draw(model: &Model, f: &mut Frame) {
    let area = f.area();
    let buf = f.buffer_mut();

    let block = Block::default().borders(Borders::all());
    let inner = block.inner(area);
    block.render(area, buf);

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    if model.clips.is_empty() {
        Line::from("No .mp4 files in this directory")
            .centered()
            .render(rows[0], buf);
        Line::from("Create or move .mp4 files here, then restart plic")
            .centered()
            .render(rows[1], buf);
        Line::from(model.status.as_str()).render(rows[8], buf);
        Line::from("q: quit").render(rows[10], buf);
        return;
    }

    let clip = model
        .current()
        .expect("selected clip exists when the list is nonempty");
    let done = model.clips.iter().filter(|clip| clip.exported).count();
    Line::from(format!(
        "Clip {}/{} · exported {done}/{}",
        model.selected + 1,
        model.clips.len(),
        model.clips.len()
    ))
    .render(rows[0], buf);
    Line::from(format!("Source: {}", display_name(&clip.path))).render(rows[1], buf);

    draw_input(
        "Name",
        &clip.name,
        rows[2],
        model.editing && model.field == EditField::Name,
        buf,
    );
    draw_input(
        "Start",
        &clip.start,
        rows[3],
        model.editing && model.field == EditField::Start,
        buf,
    );
    draw_input(
        "End",
        &clip.end,
        rows[4],
        model.editing && model.field == EditField::End,
        buf,
    );

    let folder_line = match model.folder_for(clip) {
        Some(folder) => format!(
            "Folder: {} ({}/{})",
            display_name(folder),
            clip.folder_index + 1,
            model.folders.len()
        ),
        None => "Folder: no subfolders in this directory".to_owned(),
    };
    Line::from(folder_line)
        .style(if model.editing && model.field == EditField::Folder {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default()
        })
        .render(rows[5], buf);

    let end_value = clip.end.value();
    let clip_length = match (
        parse_timestamp(&clip.start.value()),
        end_value.trim().is_empty(),
        parse_timestamp(&end_value),
    ) {
        (Some(_), true, _) => "Selected duration: until the end of the video".to_owned(),
        (Some(start), false, Some(end)) if end > start => {
            format!("Selected duration: {}", format_timestamp(end - start))
        }
        _ => "Selected duration: enter a valid start and end".to_owned(),
    };
    Line::from(clip_length).render(rows[6], buf);

    if let Some(source) = &model.exporting {
        Line::from(format!(
            "Exporting {} in the background…",
            display_name(source)
        ))
        .style(Style::default().fg(Color::Yellow))
        .render(rows[7], buf);
    }

    let help = if model.editing {
        "Tab fields · ↑/↓ folders · Enter export · Ctrl+A clear · Esc browse · Ctrl+C quit"
    } else {
        "←/→ clips · e edit · o open in default app · d delete · q quit"
    };
    Line::from(model.status.as_str()).render(rows[8], buf);
    Line::from(help).render(rows[10], buf);
}

fn draw_input(
    label: &str,
    input: &crate::text_input::TextInput,
    area: ratatui::layout::Rect,
    focused: bool,
    buf: &mut ratatui::buffer::Buffer,
) {
    let style = if focused {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(input.display(focused)),
    ])
    .style(style)
    .render(area, buf);
}

fn update(model: &mut Model, msg: Message) -> bool {
    if !matches!(msg, Message::Delete | Message::ExportFinished(_)) {
        model.pending_delete = None;
    }
    match msg {
        Message::Delete => model.delete_current(),
        Message::PreviousClip => {
            if !model.clips.is_empty() {
                model.selected = if model.selected == 0 {
                    model.clips.len() - 1
                } else {
                    model.selected - 1
                };
                model.status = format!(
                    "Selected {}",
                    display_name(&model.clips[model.selected].path)
                );
            }
        }
        Message::NextClip => {
            if !model.clips.is_empty() {
                model.selected = (model.selected + 1) % model.clips.len();
                model.status = format!(
                    "Selected {}",
                    display_name(&model.clips[model.selected].path)
                );
            }
        }
        Message::Open => {
            if let Some(path) = model.current().map(|clip| clip.path.clone()) {
                model.status = match open::that_detached(&path) {
                    Ok(()) => format!("Opened {}", display_name(&path)),
                    Err(error) => format!("Could not open video: {error}"),
                };
            }
        }
        Message::BeginEdit => {
            model.editing = true;
            model.field = EditField::Name;
            model.status =
                "Edit the name and time range, choose a folder, then press Enter".to_owned();
        }
        Message::CancelEdit => {
            model.editing = false;
            model.status = "Returned to clip browsing; edits are kept".to_owned();
        }
        Message::FocusNext => model.field = model.field.next(),
        Message::FocusPrevious => model.field = model.field.previous(),
        Message::FolderPrevious => move_folder(model, false),
        Message::FolderNext => move_folder(model, true),
        Message::CursorLeft => {
            with_current_input(model, |input| input.move_left());
        }
        Message::CursorRight => {
            with_current_input(model, |input| input.move_right());
        }
        Message::Backspace => {
            with_current_input(model, |input| input.backspace());
        }
        Message::Insert(character) => {
            if !with_current_input(model, |input| input.insert(character)).unwrap_or(false) {
                model.status = "Input limit reached or character is not allowed".to_owned();
            }
        }
        Message::ClearField => {
            with_current_input(model, TextInput::clear);
        }
        Message::Export => start_export(model),
        Message::ExportFinished(result) => model.finish_export(result),
        Message::Quit => return false,
    }

    true
}

fn with_current_input<T>(
    model: &mut Model,
    edit: impl FnOnce(&mut crate::text_input::TextInput) -> T,
) -> Option<T> {
    let field = model.field;
    if let Some(clip) = model.current_mut() {
        let input = match field {
            EditField::Name => &mut clip.name,
            EditField::Start => &mut clip.start,
            EditField::End => &mut clip.end,
            EditField::Folder => return None,
        };
        Some(edit(input))
    } else {
        None
    }
}

fn move_folder(model: &mut Model, forward: bool) {
    if model.folders.is_empty() {
        model.status = "Create a subfolder to use it as an export destination".to_owned();
        return;
    }
    let count = model.folders.len();
    if let Some(clip) = model.current_mut() {
        clip.folder_index = if forward {
            (clip.folder_index + 1) % count
        } else if clip.folder_index == 0 {
            count - 1
        } else {
            clip.folder_index - 1
        };
    }
}

fn start_export(model: &mut Model) {
    if model.exporting.is_some() {
        model.status = "An export is already running".to_owned();
        return;
    }

    let Some(clip) = model.current() else {
        return;
    };
    let source = clip.path.clone();
    let name = clip.name.value().trim().to_owned();
    let start_text = clip.start.value().trim().to_owned();
    let end_text = clip.end.value().trim().to_owned();
    let Some(folder) = model.folder_for(clip).map(Path::to_path_buf) else {
        model.status = "Create a subfolder and select it before exporting".to_owned();
        return;
    };

    if !valid_file_stem(&name) {
        model.status = "Enter a valid output name without path or reserved characters".to_owned();
        return;
    }
    let Some(start_seconds) = parse_timestamp(&start_text) else {
        model.status = "Start must use MM:SS or HH:MM:SS, for example 2:30".to_owned();
        return;
    };
    let end_seconds = if end_text.is_empty() {
        None
    } else {
        let Some(seconds) = parse_timestamp(&end_text) else {
            model.status = "End must use MM:SS or HH:MM:SS, for example 4:21".to_owned();
            return;
        };
        if seconds <= start_seconds {
            model.status = "End time must be later than start time".to_owned();
            return;
        }
        Some(seconds)
    };

    let output = folder.join(format!("{name}.mp4"));
    if output.exists() {
        model.status = format!("{} already exists; choose another name", output.display());
        return;
    }

    let start = (start_seconds > 0).then_some(start_text);
    let duration = end_seconds.map(|end| format_timestamp(end - start_seconds));
    model.start_export(source, output, start, duration);
}

fn valid_file_stem(name: &str) -> bool {
    let device_name = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let is_reserved_device = matches!(device_name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            device_name.strip_prefix(prefix).is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
        });

    !is_reserved_device
        && !name.is_empty()
        && name != "."
        && name != ".."
        && !matches!(name.chars().last(), Some(' ' | '.'))
        && !name
            .chars()
            .any(|character| character.is_control() || "<>:\"/\\|?*".contains(character))
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
