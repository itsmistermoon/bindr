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
const PAGE_STEP: usize = 10;

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

/// Case-insensitive subsequence match, fzf-style but without the ranking.
fn fuzzy_match(text: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let text_lower = text.to_lowercase();
    let mut chars = text_lower.chars();
    'query: for qc in query.to_lowercase().chars() {
        for tc in chars.by_ref() {
            if tc == qc {
                continue 'query;
            }
        }
        return false;
    }
    true
}

/// Indices into `rows` to display for the given query: every matching entry,
/// plus the section header above it (once) for context. A section whose own
/// name matches shows all of its entries. An empty query shows everything,
/// blanks included.
fn filtered_indices(rows: &[Row], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..rows.len()).collect();
    }
    let mut result = Vec::new();
    let mut section_start: Option<usize> = None;
    let mut section_matches = false;
    let mut section_included = false;
    for (i, row) in rows.iter().enumerate() {
        match row {
            Row::Section(name) => {
                section_start = Some(i);
                section_matches = fuzzy_match(name, query);
                section_included = section_matches;
                if section_matches {
                    result.push(i);
                }
            }
            Row::Blank => {}
            Row::Entry(key, desc) => {
                if section_matches || fuzzy_match(key, query) || fuzzy_match(desc, query) {
                    if !section_included {
                        if let Some(si) = section_start {
                            result.push(si);
                        }
                        section_included = true;
                    }
                    result.push(i);
                }
            }
        }
    }
    result
}

/// (label, keys) pairs for the footer hint, in display order. Kept whole --
/// never broken across a line -- by `wrap_footer`.
fn footer_segments(searching: bool) -> Vec<(&'static str, &'static str)> {
    if searching {
        vec![
            ("filter", "type/backspace"),
            ("clear", "ctrl+u"),
            ("scroll", "\u{2191}\u{2193}/pgup/pgdn"),
            ("back", "esc"),
        ]
    } else {
        vec![
            ("search", "/"),
            ("scroll", "j/k/\u{2191}\u{2193}/pgup/pgdn"),
            ("switch profile", "shift+k"),
            ("close", "esc/enter/q"),
        ]
    }
}

const FOOTER_SEP: &str = " \u{b7} ";

/// Greedily packs footer segments onto lines, each no wider than `cols`
/// (accounting for `LEFT_PAD`), without ever splitting a single segment.
fn wrap_footer(segments: &[(&str, &str)], cols: usize) -> Vec<String> {
    let budget = cols.saturating_sub(LEFT_PAD.chars().count());
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_width = 0;
    for (label, key) in segments {
        let seg_width = label.chars().count() + 1 + key.chars().count();
        let extra = if line.is_empty() { 0 } else { FOOTER_SEP.chars().count() };
        if !line.is_empty() && line_width + extra + seg_width > budget {
            lines.push(std::mem::take(&mut line));
            line_width = 0;
        }
        if !line.is_empty() {
            line.push_str(&format!("{DIM}{FOOTER_SEP}{RESET}"));
            line_width += FOOTER_SEP.chars().count();
        }
        line.push_str(&format!("{DIM}{label}{RESET} {key}"));
        line_width += seg_width;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn render_row(row: &Row, width: usize) -> String {
    match row {
        Row::Blank => String::new(),
        Row::Section(name) => format!("{LEFT_PAD}{BOLD}{CYAN}{name}{RESET}"),
        Row::Entry(key, desc) => {
            let desc_width = (width.saturating_sub(LEFT_PAD.len() + KEY_COL)).max(4);
            let desc = if desc.chars().count() > desc_width {
                let mut cut: String = desc.chars().take(desc_width.saturating_sub(1)).collect();
                if let Some(space_idx) = cut.rfind(' ')
                    && space_idx as f64 > desc_width as f64 * 0.6 {
                        cut.truncate(space_idx);
                    }
                format!("{}…", cut.trim_end())
            } else {
                desc.clone()
            };
            format!("{LEFT_PAD}{BOLD}{key:<KEY_COL$}{RESET}{desc}")
        }
    }
}

/// Everything the popup needs to redraw itself: the full row list, the
/// current search's visible subset of it, and cursor/mode state. Bundled so
/// `render` takes one argument instead of a growing parameter list.
struct ViewState {
    rows: Vec<Row>,
    filtered: Vec<usize>,
    active_profile: String,
    offset: usize,
    query: String,
    searching: bool,
}

impl ViewState {
    fn load() -> Result<Self> {
        let rows = build_rows()?;
        let query = String::new();
        let filtered = filtered_indices(&rows, &query);
        Ok(Self {
            rows,
            filtered,
            active_profile: config::read_active_profile().unwrap_or_else(|| "?".to_string()),
            offset: 0,
            query,
            searching: false,
        })
    }

    /// Recompute `filtered` from the current query and jump back to the top.
    /// `filtered` and `offset` always change together, so this is the only
    /// place either is touched outside scrolling.
    fn refilter(&mut self) {
        self.filtered = filtered_indices(&self.rows, &self.query);
        self.offset = 0;
    }

    fn switch_profile(&mut self) -> Result<()> {
        if switch::switch_to_next().is_ok() {
            self.rows = build_rows()?;
            self.active_profile = config::read_active_profile().unwrap_or_else(|| "?".to_string());
            self.filtered = filtered_indices(&self.rows, &self.query);
        }
        Ok(())
    }
}

fn render(out: &mut impl Write, state: &mut ViewState) -> Result<()> {
    let (cols, term_rows) = terminal::size()?;
    let cols = cols as usize;

    let footer_lines = wrap_footer(&footer_segments(state.searching), cols);
    // Fixed chrome: banner + blank, search/hint line + blank, trailing blank,
    // then the footer (which may itself wrap onto more than one line).
    let viewport = (term_rows as usize)
        .saturating_sub(5 + footer_lines.len())
        .max(1);
    let max_offset = state.filtered.len().saturating_sub(viewport);
    state.offset = state.offset.min(max_offset);

    let banner = format!(" active profile: {} ", state.active_profile);
    let pad = cols.saturating_sub(banner.chars().count()) / 2;

    let mut buf = String::new();
    buf.push_str("\x1b[2J\x1b[H");
    buf.push_str(&" ".repeat(pad));
    buf.push_str(&format!("{BOLD}{BG_CYAN}{FG_BLACK}{banner}{RESET}\x1b[K\r\n\r\n"));
    if state.searching {
        buf.push_str(&format!(
            "{LEFT_PAD}{BOLD}/{RESET}{}\u{2588}\x1b[K\r\n\r\n",
            state.query
        ));
    } else {
        buf.push_str(&format!(
            "{LEFT_PAD}{DIM}press / to filter by command or shortcut{RESET}\x1b[K\r\n\r\n"
        ));
    }
    if state.filtered.is_empty() {
        buf.push_str(&format!("{LEFT_PAD}{DIM}no matches{RESET}\x1b[K\r\n"));
    }
    for &idx in state.filtered.iter().skip(state.offset).take(viewport) {
        buf.push_str(&render_row(&state.rows[idx], cols));
        buf.push_str("\x1b[K\r\n");
    }
    buf.push_str("\x1b[K\r\n");
    for (i, line) in footer_lines.iter().enumerate() {
        if i > 0 {
            buf.push_str("\x1b[K\r\n");
        }
        buf.push_str(LEFT_PAD);
        buf.push_str(line);
    }
    buf.push_str("\x1b[K");
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
    let mut state = ViewState::load()?;

    loop {
        render(out, &mut state)?;

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
                    MouseEventKind::ScrollUp => state.offset = state.offset.saturating_sub(3),
                    MouseEventKind::ScrollDown => state.offset += 3,
                    _ => {}
                },
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        quit = true;
                    } else if state.searching {
                        match k.code {
                            KeyCode::Esc => {
                                state.searching = false;
                                state.query.clear();
                                state.refilter();
                            }
                            KeyCode::Enter => state.searching = false,
                            // Arrow keys navigate the live-filtered results
                            // without leaving search mode; letters (including
                            // j/k) stay reserved for the query text.
                            KeyCode::Down => state.offset += 1,
                            KeyCode::Up => state.offset = state.offset.saturating_sub(1),
                            KeyCode::PageDown => state.offset += PAGE_STEP,
                            KeyCode::PageUp => {
                                state.offset = state.offset.saturating_sub(PAGE_STEP)
                            }
                            KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                                state.query.clear();
                                state.refilter();
                            }
                            KeyCode::Backspace => {
                                state.query.pop();
                                state.refilter();
                            }
                            KeyCode::Char(c) => {
                                state.query.push(c);
                                state.refilter();
                            }
                            _ => {}
                        }
                    } else {
                        match k.code {
                            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => quit = true,
                            KeyCode::Char('/') => state.searching = true,
                            KeyCode::Char('j') | KeyCode::Down => state.offset += 1,
                            KeyCode::Char('k') | KeyCode::Up => {
                                state.offset = state.offset.saturating_sub(1)
                            }
                            KeyCode::PageDown => state.offset += PAGE_STEP,
                            KeyCode::PageUp => {
                                state.offset = state.offset.saturating_sub(PAGE_STEP)
                            }
                            KeyCode::Char('K') => state.switch_profile()?,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        if quit {
            break;
        }
    }
    Ok(())
}
