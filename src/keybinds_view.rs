//! Read-only replica of Herdr's built-in prefix+? keybinds panel, plus one
//! interactive extra: shift+K switches the active profile in place so the
//! effect is visible live, without leaving the popup.

use crate::{config, keybinds_data, keys, switch};
use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
};
use crossterm::{cursor, execute, terminal};
use std::collections::HashMap;
use std::io::{stdout, Write};
use std::time::Duration;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const BG_CYAN: &str = "\x1b[46m";
const FG_BLACK: &str = "\x1b[30m";
const KEY_COL: usize = 30;
const LEFT_PAD: &str = "  ";

enum Row {
    Section(String),
    Blank,
    Entry(String, String),
}

fn display_value(v: &str) -> String {
    if v.is_empty() {
        return "unset".to_string();
    }
    v.split('+')
        .map(|seg| if seg == "minus" { "-" } else { seg })
        .collect::<Vec<_>>()
        .join("+")
}

fn build_rows() -> Result<Vec<Row>> {
    let doc = keys::load(&config::config_path())?;
    let overrides: HashMap<String, String> = keys::scalar_overrides(&doc).into_iter().collect();
    let custom = keys::custom_commands(&doc);

    let mut rows = Vec::new();
    for section in keybinds_data::SECTIONS {
        rows.push(Row::Section(section.name.to_string()));
        for r in section.rows {
            let value = match r.config_key {
                Some(k) => overrides.get(k).map(String::as_str).unwrap_or(r.default),
                None => r.default,
            };
            rows.push(Row::Entry(display_value(value), r.description.to_string()));
        }
        rows.push(Row::Blank);
    }

    rows.push(Row::Section("custom".to_string()));
    for (key, desc) in custom {
        rows.push(Row::Entry(display_value(&key), desc));
    }
    Ok(rows)
}

fn render_row(row: &Row, width: usize) -> String {
    match row {
        Row::Blank => String::new(),
        Row::Section(name) => format!("{LEFT_PAD}{BOLD}{CYAN}{name}{RESET}"),
        Row::Entry(key, desc) => {
            let desc_width = (width.saturating_sub(LEFT_PAD.len() + KEY_COL)).max(4);
            let desc = if desc.chars().count() > desc_width {
                let mut cut: String = desc.chars().take(desc_width.saturating_sub(1)).collect();
                if let Some(space_idx) = cut.rfind(' ') {
                    if space_idx as f64 > desc_width as f64 * 0.6 {
                        cut.truncate(space_idx);
                    }
                }
                format!("{}…", cut.trim_end())
            } else {
                desc.clone()
            };
            format!("{LEFT_PAD}{BOLD}{key:<KEY_COL$}{RESET}{desc}")
        }
    }
}

fn render(
    out: &mut impl Write,
    rows: &[Row],
    active_profile: &str,
    offset: &mut usize,
) -> Result<()> {
    let (cols, term_rows) = terminal::size()?;
    let cols = cols as usize;
    let viewport = (term_rows as usize).saturating_sub(4).max(1);
    let max_offset = rows.len().saturating_sub(viewport);
    *offset = (*offset).min(max_offset);

    let banner = format!(" active profile: {active_profile} ");
    let pad = cols.saturating_sub(banner.chars().count()) / 2;

    let mut buf = String::new();
    buf.push_str("\x1b[2J\x1b[H");
    buf.push_str(&" ".repeat(pad));
    buf.push_str(&format!("{BOLD}{BG_CYAN}{FG_BLACK}{banner}{RESET}\x1b[K\r\n\r\n"));
    for row in rows.iter().skip(*offset).take(viewport) {
        buf.push_str(&render_row(row, cols));
        buf.push_str("\x1b[K\r\n");
    }
    buf.push_str("\x1b[K\r\n");
    buf.push_str(&format!(
        "{LEFT_PAD}{DIM}scroll{RESET} {BOLD}j/k/wheel/\u{2191}/\u{2193}{RESET}\
         {DIM}  \u{b7}  switch profile{RESET} {BOLD}shift+k{RESET}\
         {DIM}  \u{b7}  close{RESET} {BOLD}esc/q{RESET}\x1b[K"
    ));
    out.write_all(buf.as_bytes())?;
    out.flush()?;
    Ok(())
}

pub fn run() -> Result<()> {
    terminal::enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, event::EnableMouseCapture, cursor::Hide)?;

    let result = main_loop(&mut out);

    let _ = execute!(out, event::DisableMouseCapture, cursor::Show);
    let _ = terminal::disable_raw_mode();
    result
}

fn main_loop(out: &mut impl Write) -> Result<()> {
    let mut rows = build_rows()?;
    let mut active_profile = config::read_active_profile().unwrap_or_else(|| "?".to_string());
    let mut offset: usize = 0;

    render(out, &rows, &active_profile, &mut offset)?;

    loop {
        // Block for the next event, then drain any already-queued ones (a
        // fast scroll burst) before redrawing once, instead of redrawing
        // per event.
        let mut events = vec![event::read()?];
        while event::poll(Duration::from_millis(0))? {
            events.push(event::read()?);
        }

        let mut quit = false;
        for ev in events {
            match ev {
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::ScrollUp => offset = offset.saturating_sub(3),
                    MouseEventKind::ScrollDown => offset += 3,
                    _ => {}
                },
                Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                    KeyCode::Esc | KeyCode::Char('q') => quit = true,
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        quit = true
                    }
                    KeyCode::Char('j') | KeyCode::Down => offset += 1,
                    KeyCode::Char('k') | KeyCode::Up => offset = offset.saturating_sub(1),
                    KeyCode::Char('K') => {
                        if switch::switch_to_next().is_ok() {
                            rows = build_rows()?;
                            active_profile =
                                config::read_active_profile().unwrap_or_else(|| "?".to_string());
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        if quit {
            break;
        }
        render(out, &rows, &active_profile, &mut offset)?;
    }
    Ok(())
}
