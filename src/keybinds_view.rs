//! Replica of Herdr's built-in prefix+? keybinds panel. Profiles are shown as
//! tabs: selecting a tab only changes the profile being viewed; shift+K still
//! switches the active profile in Herdr without changing the selected tab.

use crate::{config, keybinds_data, keys, switch};
use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::{cursor, execute, terminal};
use std::collections::HashMap;
use std::io::{Write, stdout};
use std::time::Duration;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const BG_CYAN: &str = "\x1b[46m";
const FG_BLACK: &str = "\x1b[30m";
// Use standard white plus bold instead of bright-white (97) alone. The
// latter can collapse to the normal foreground in remote/limited-color PTYs.
const FG_WHITE: &str = "\x1b[1;37m";
const KEY_COL: usize = 30;
const LEFT_PAD: &str = "  ";
const PAGE_STEP: usize = 10;
const TABS_ROW: u16 = 1;

enum Row {
    Section(String),
    Blank,
    Entry {
        config_key: Option<String>,
        key: String,
        description: String,
    },
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

fn build_rows(
    profile_doc: &toml_edit::DocumentMut,
    custom_doc: &toml_edit::DocumentMut,
) -> Vec<Row> {
    let overrides: HashMap<String, String> =
        keys::scalar_overrides(profile_doc).into_iter().collect();
    let custom = keys::custom_commands(custom_doc);

    let mut rows = Vec::new();
    for section in keybinds_data::SECTIONS {
        rows.push(Row::Section(section.name.to_string()));
        for r in section.rows {
            let value = match r.config_key {
                Some(k) => overrides.get(k).map(String::as_str).unwrap_or(r.default),
                None => r.default,
            };
            rows.push(Row::Entry {
                config_key: r.config_key.map(str::to_string),
                key: display_value(value),
                description: r.description.to_string(),
            });
        }
        rows.push(Row::Blank);
    }

    rows.push(Row::Section("custom".to_string()));
    for (key, desc) in custom {
        rows.push(Row::Entry {
            config_key: None,
            key: display_value(&key),
            description: desc,
        });
    }
    rows
}

fn build_rows_from_live_config() -> Result<Vec<Row>> {
    let doc = keys::load(&config::config_path())?;
    Ok(build_rows(&doc, &doc))
}

fn build_rows_for_profile(name: &str) -> Result<Vec<Row>> {
    let profile_path = config::profiles_dir().join(format!("{name}.toml"));
    let profile = keys::load(&profile_path)?;
    let live = keys::load(&config::config_path())?;
    Ok(build_rows(&profile, &live))
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
            Row::Entry {
                key, description, ..
            } => {
                if section_matches || fuzzy_match(key, query) || fuzzy_match(description, query) {
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

/// One "label key" pair in the footer hint, e.g. label "close", key
/// "esc/enter/q". Kept whole -- never broken across a line -- by `wrap_footer`.
struct FooterHint {
    label: &'static str,
    key: &'static str,
}

fn footer_segments(
    searching: bool,
    editing: bool,
    listening: bool,
    prefix_listening: bool,
) -> Vec<FooterHint> {
    if searching {
        vec![
            FooterHint {
                label: "filter",
                key: "type/backspace",
            },
            FooterHint {
                label: "clear",
                key: "ctrl+u",
            },
            FooterHint {
                label: "scroll",
                key: "\u{2191}\u{2193}/pgup/pgdn",
            },
            FooterHint {
                label: "back",
                key: "esc",
            },
        ]
    } else if listening {
        vec![
            FooterHint {
                label: "capture",
                key: "direct/prefix",
            },
            FooterHint {
                label: "save",
                key: "enter",
            },
            FooterHint {
                label: "retry",
                key: "backspace",
            },
            FooterHint {
                label: "cancel",
                key: if prefix_listening {
                    "esc/ctrl+c"
                } else {
                    "esc"
                },
            },
        ]
    } else if editing {
        vec![
            FooterHint {
                label: "select",
                key: "j/k/\u{2191}\u{2193}",
            },
            FooterHint {
                label: "listen",
                key: "enter",
            },
            FooterHint {
                label: "back",
                key: "esc",
            },
        ]
    } else {
        vec![
            FooterHint {
                label: "edit",
                key: "e",
            },
            FooterHint {
                label: "undo",
                key: "u",
            },
            FooterHint {
                label: "search",
                key: "/",
            },
            FooterHint {
                label: "tabs",
                key: "\u{2190}/\u{2192}/click",
            },
            FooterHint {
                label: "scroll",
                key: "j/k/\u{2191}\u{2193}/pgup/pgdn",
            },
            FooterHint {
                label: "switch active profile",
                key: "shift+k",
            },
            FooterHint {
                label: "close",
                key: "esc/enter/q",
            },
        ]
    }
}

const FOOTER_SEP: &str = " \u{b7} ";

fn tab_label(name: &str, selected: bool, active: bool) -> String {
    let marker = if active { "*" } else { "" };
    if selected {
        format!("[{marker}{name}]")
    } else {
        format!("{marker}{name}")
    }
}

fn styled_tab(name: &str, selected: bool, active: bool) -> String {
    let label = tab_label(name, selected, active);
    if selected {
        format!("{BOLD}{BG_CYAN}{FG_BLACK}{label}{RESET}")
    } else if active {
        format!("{BOLD}{CYAN}{label}{RESET}")
    } else {
        format!("{DIM}{label}{RESET}")
    }
}

/// Return the inclusive-exclusive range of tabs that fits on the single tab
/// row. The selected tab is always kept visible when the list is too wide.
fn visible_tab_range(
    profiles: &[String],
    viewed: Option<&str>,
    active: &str,
    cols: usize,
) -> (usize, usize) {
    if profiles.is_empty() {
        return (0, 0);
    }

    let selected = viewed
        .and_then(|name| profiles.iter().position(|profile| profile == name))
        .unwrap_or(0);
    let budget = cols.saturating_sub(LEFT_PAD.chars().count());
    let mut start = 0;
    let mut end = profiles.len();

    let strip_width = |start: usize, end: usize| {
        let tabs_width: usize = profiles[start..end]
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let selected = start + i == selected;
                tab_label(name, selected, name == active).chars().count()
            })
            .sum();
        let separators = end.saturating_sub(start + 1);
        let edge_markers = usize::from(start > 0) * 2 + usize::from(end < profiles.len()) * 2;
        tabs_width + separators + edge_markers
    };

    while strip_width(start, end) > budget && (start < selected || end > selected + 1) {
        if start < selected {
            start += 1;
        } else {
            end -= 1;
        }
    }
    (start, end)
}

/// Return the profile tab under a terminal column, if that tab is visible.
fn tab_at_column(
    profiles: &[String],
    viewed: Option<&str>,
    active: &str,
    cols: usize,
    column: u16,
) -> Option<usize> {
    let (start, end) = visible_tab_range(profiles, viewed, active, cols);
    let selected = viewed
        .and_then(|name| profiles.iter().position(|profile| profile == name))
        .unwrap_or(0);
    let mut x = LEFT_PAD.chars().count() + usize::from(start > 0) * 2;
    for (i, name) in profiles[start..end].iter().enumerate() {
        let index = start + i;
        let width = tab_label(name, index == selected, name == active)
            .chars()
            .count();
        if usize::from(column) >= x && usize::from(column) < x + width {
            return Some(index);
        }
        x += width + 1;
    }
    None
}

/// Render a single-line tab strip. If the profile list is wider than the
/// popup, keep the selected tab visible and elide tabs at either edge rather
/// than allowing the terminal to wrap the strip into the viewport.
fn render_tabs(profiles: &[String], viewed: Option<&str>, active: &str, cols: usize) -> String {
    if profiles.is_empty() {
        return format!("{DIM}no profiles{RESET}");
    }

    let selected = viewed
        .and_then(|name| profiles.iter().position(|profile| profile == name))
        .unwrap_or(0);
    let (start, end) = visible_tab_range(profiles, viewed, active, cols);

    let mut line = String::new();
    if start > 0 {
        line.push_str(&format!("{DIM}\u{2026} {RESET}"));
    }
    for (i, name) in profiles[start..end].iter().enumerate() {
        if i > 0 {
            line.push(' ');
        }
        let is_selected = start + i == selected;
        line.push_str(&styled_tab(name, is_selected, name == active));
    }
    if end < profiles.len() {
        line.push_str(&format!(" {DIM}\u{2026}{RESET}"));
    }
    line
}

/// Greedily packs footer segments onto lines, each no wider than `cols`
/// (accounting for `LEFT_PAD`), without ever splitting a single segment.
fn wrap_footer(segments: &[FooterHint], cols: usize) -> Vec<String> {
    let budget = cols.saturating_sub(LEFT_PAD.chars().count());
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_width = 0;
    for hint in segments {
        let seg_width = hint.label.chars().count() + 1 + hint.key.chars().count();
        let extra = if line.is_empty() {
            0
        } else {
            FOOTER_SEP.chars().count()
        };
        if !line.is_empty() && line_width + extra + seg_width > budget {
            lines.push(std::mem::take(&mut line));
            line_width = 0;
        }
        if !line.is_empty() {
            line.push_str(&format!("{DIM}{FOOTER_SEP}{RESET}"));
            line_width += FOOTER_SEP.chars().count();
        }
        line.push_str(&format!(
            "{DIM}{}{RESET} {FG_WHITE}{}{RESET}",
            hint.label, hint.key
        ));
        line_width += seg_width;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[derive(Clone, Copy)]
enum EditMode {
    Selecting { row: usize },
    Listening { row: usize, prefix_seen: bool },
}

fn row_config_key(row: &Row) -> Option<&str> {
    match row {
        Row::Entry {
            config_key: Some(key),
            ..
        } => Some(key),
        _ => None,
    }
}

fn key_event_binding(key: &KeyEvent) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl".to_string());
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        parts.push("alt".to_string());
    }
    if key.modifiers.contains(KeyModifiers::SUPER) {
        parts.push("super".to_string());
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        parts.push("shift".to_string());
    }

    let code = match key.code {
        KeyCode::Char(c) => match c {
            ' ' => "space".to_string(),
            '-' => "minus".to_string(),
            ',' => "comma".to_string(),
            '&' => "ampersand".to_string(),
            '+' => "plus".to_string(),
            '`' => "backtick".to_string(),
            c if c.is_ascii_alphabetic() => c.to_ascii_lowercase().to_string(),
            c => c.to_string(),
        },
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Tab | KeyCode::BackTab => "tab".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "pageup".to_string(),
        KeyCode::PageDown => "pagedown".to_string(),
        KeyCode::Delete => "delete".to_string(),
        KeyCode::Insert => "insert".to_string(),
        KeyCode::F(number) => format!("f{number}"),
        _ => return None,
    };
    parts.push(code);
    Some(parts.join("+"))
}

fn default_binding(config_key: &str) -> Option<&'static str> {
    keybinds_data::SECTIONS
        .iter()
        .flat_map(|section| section.rows)
        .find(|row| row.config_key == Some(config_key))
        .map(|row| row.default)
}

fn profile_binding(profile_name: &str, config_key: &str) -> Result<Option<String>> {
    let path = config::profiles_dir().join(format!("{profile_name}.toml"));
    let doc = keys::load(&path)?;
    let value = keys::scalar_overrides(&doc)
        .into_iter()
        .find(|(key, _)| key == config_key)
        .map(|(_, value)| value)
        .or_else(|| default_binding(config_key).map(str::to_string));
    Ok(value)
}

struct UndoEntry {
    profile_name: String,
    config_key: String,
    profile_previous: Option<toml_edit::Item>,
    live_previous: Option<toml_edit::Item>,
}

fn persist_binding(
    profile_name: &str,
    active_profile: &str,
    config_key: &str,
    binding: &str,
) -> Result<UndoEntry> {
    let profile_path = config::profiles_dir().join(format!("{profile_name}.toml"));
    let mut profile = keys::load(&profile_path)?;
    let profile_previous = keys::get_item(&profile, config_key);
    keys::set_scalar(&mut profile, config_key, binding);
    keys::save(&profile_path, &profile)?;

    let live_previous = if profile_name == active_profile {
        let config_path = config::config_path();
        let mut live = keys::load(&config_path)?;
        let previous = keys::get_item(&live, config_key);
        keys::set_scalar(&mut live, config_key, binding);
        keys::save(&config_path, &live)?;
        switch::reload_config()?;
        previous
    } else {
        None
    };
    Ok(UndoEntry {
        profile_name: profile_name.to_string(),
        config_key: config_key.to_string(),
        profile_previous,
        live_previous,
    })
}

fn restore_binding(undo: &UndoEntry, active_profile: &str) -> Result<()> {
    let profile_path = config::profiles_dir().join(format!("{}.toml", undo.profile_name));
    let mut profile = keys::load(&profile_path)?;
    match &undo.profile_previous {
        Some(item) => keys::set_item(&mut profile, &undo.config_key, item.clone()),
        None => keys::remove_scalar(&mut profile, &undo.config_key),
    }
    keys::save(&profile_path, &profile)?;

    if undo.profile_name == active_profile {
        let config_path = config::config_path();
        let mut live = keys::load(&config_path)?;
        match &undo.live_previous {
            Some(item) => keys::set_item(&mut live, &undo.config_key, item.clone()),
            None => keys::remove_scalar(&mut live, &undo.config_key),
        }
        keys::save(&config_path, &live)?;
        switch::reload_config()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_keys_use_a_remote_safe_emphasis() {
        let lines = wrap_footer(&footer_segments(false, false, false, false), 76);
        assert!(!lines.is_empty());
        assert!(lines.iter().all(|line| line.contains(FG_WHITE)));
    }

    #[test]
    fn key_event_binding_uses_herdr_names() {
        let key = KeyEvent::new(
            KeyCode::Char('R'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(key_event_binding(&key).as_deref(), Some("ctrl+shift+r"));
    }
}

fn render_row(row: &Row, width: usize, selected: bool) -> String {
    match row {
        Row::Blank => String::new(),
        Row::Section(name) => format!("{LEFT_PAD}{BOLD}{CYAN}{name}{RESET}"),
        Row::Entry {
            key, description, ..
        } => {
            let desc_width = (width.saturating_sub(LEFT_PAD.len() + KEY_COL)).max(4);
            let desc = if description.chars().count() > desc_width {
                let mut cut: String = description
                    .chars()
                    .take(desc_width.saturating_sub(1))
                    .collect();
                if let Some(space_idx) = cut.rfind(' ')
                    && space_idx as f64 > desc_width as f64 * 0.6
                {
                    cut.truncate(space_idx);
                }
                format!("{}…", cut.trim_end())
            } else {
                description.clone()
            };
            if selected {
                format!(
                    "{BG_CYAN}{FG_BLACK}{LEFT_PAD}{BOLD}{key:<KEY_COL$}{RESET}{BG_CYAN}{FG_BLACK}{desc}{RESET}"
                )
            } else {
                format!("{LEFT_PAD}{BOLD}{key:<KEY_COL$}{RESET}{desc}")
            }
        }
    }
}

/// Everything the popup needs to redraw itself: the full row list, the
/// current search's visible subset of it, and cursor/mode state. Bundled so
/// `render` takes one argument instead of a growing parameter list.
struct ViewState {
    profiles: Vec<String>,
    viewed_profile: Option<String>,
    rows: Vec<Row>,
    filtered: Vec<usize>,
    active_profile: String,
    offset: usize,
    query: String,
    searching: bool,
    editing: Option<EditMode>,
    pending_binding: Option<String>,
    last_undo: Option<UndoEntry>,
}

impl ViewState {
    fn load() -> Result<Self> {
        let profiles = config::list_profiles()?;
        let active_profile = config::read_active_profile().unwrap_or_else(|| "?".to_string());
        let viewed_profile = profiles
            .iter()
            .find(|name| name.as_str() == active_profile)
            .cloned()
            .or_else(|| profiles.first().cloned());
        let rows = match viewed_profile.as_deref() {
            Some(name) => build_rows_for_profile(name)?,
            None => build_rows_from_live_config()?,
        };
        let query = String::new();
        let filtered = filtered_indices(&rows, &query);
        Ok(Self {
            profiles,
            viewed_profile,
            rows,
            filtered,
            active_profile,
            offset: 0,
            query,
            searching: false,
            editing: None,
            pending_binding: None,
            last_undo: None,
        })
    }

    fn reload_viewed(&mut self) -> Result<()> {
        let offset = self.offset;
        self.rows = match self.viewed_profile.as_deref() {
            Some(name) => build_rows_for_profile(name)?,
            None => build_rows_from_live_config()?,
        };
        self.filtered = filtered_indices(&self.rows, &self.query);
        self.offset = offset;
        Ok(())
    }

    fn select_profile(&mut self, index: usize) -> Result<()> {
        if self.profiles.get(index).is_none() {
            return Ok(());
        }
        self.viewed_profile = Some(self.profiles[index].clone());
        self.reload_viewed()
    }

    fn move_profile(&mut self, delta: isize) -> Result<()> {
        if self.profiles.is_empty() {
            return Ok(());
        }
        let current = self
            .viewed_profile
            .as_deref()
            .and_then(|name| self.profiles.iter().position(|profile| profile == name))
            .unwrap_or(0);
        let next = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            (current + delta as usize).min(self.profiles.len() - 1)
        };
        self.select_profile(next)
    }

    /// Recompute `filtered` from the current query and jump back to the top.
    /// Profile changes use `reload_viewed` instead so comparison scrolling is
    /// preserved.
    fn refilter(&mut self) {
        self.filtered = filtered_indices(&self.rows, &self.query);
        self.offset = 0;
    }

    /// Move the scroll offset by `delta` rows, clamping at zero.
    fn scroll(&mut self, delta: isize) {
        self.offset = if delta < 0 {
            self.offset.saturating_sub(delta.unsigned_abs())
        } else {
            self.offset + delta as usize
        };
    }

    fn switch_profile(&mut self) -> Result<()> {
        if switch::switch_to_next().is_ok() {
            self.active_profile = config::read_active_profile().unwrap_or_else(|| "?".to_string());
            self.profiles = config::list_profiles()?;
        }
        Ok(())
    }

    fn first_editable_row(&self) -> Option<usize> {
        self.filtered
            .iter()
            .copied()
            .find(|&index| row_config_key(&self.rows[index]).is_some())
    }

    fn begin_edit(&mut self) {
        if self.viewed_profile.is_some() {
            self.searching = false;
            self.query.clear();
            self.refilter();
            if let Some(row) = self.first_editable_row() {
                self.editing = Some(EditMode::Selecting { row });
                self.keep_edit_row_visible(row);
            }
        }
    }

    fn keep_edit_row_visible(&mut self, row: usize) {
        let Some(position) = self.filtered.iter().position(|&index| index == row) else {
            return;
        };
        let (cols, term_rows) = terminal::size().unwrap_or((76, 22));
        let footer_lines =
            wrap_footer(&footer_segments(false, true, false, false), cols as usize).len();
        let viewport = (term_rows as usize).saturating_sub(6 + footer_lines).max(1);
        if position < self.offset {
            self.offset = position;
        } else if position >= self.offset + viewport {
            self.offset = position + 1 - viewport;
        }
    }

    fn move_edit_selection(&mut self, delta: isize) {
        let Some(EditMode::Selecting { row }) = self.editing else {
            return;
        };
        let editable: Vec<usize> = self
            .filtered
            .iter()
            .copied()
            .filter(|&index| row_config_key(&self.rows[index]).is_some())
            .collect();
        let Some(current) = editable.iter().position(|&index| index == row) else {
            return;
        };
        let next = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            (current + delta as usize).min(editable.len().saturating_sub(1))
        };
        let row = editable[next];
        self.editing = Some(EditMode::Selecting { row });
        self.keep_edit_row_visible(row);
    }

    fn listen_selected(&mut self) {
        if let Some(EditMode::Selecting { row }) = self.editing {
            self.pending_binding = None;
            self.editing = Some(EditMode::Listening {
                row,
                prefix_seen: false,
            });
        }
    }

    fn save_edit(&mut self, row: usize, binding: &str) -> Result<()> {
        let Some(config_key) = row_config_key(&self.rows[row]) else {
            return Ok(());
        };
        let Some(profile_name) = self.viewed_profile.clone() else {
            return Ok(());
        };
        let undo = persist_binding(&profile_name, &self.active_profile, config_key, binding)?;
        self.last_undo = Some(undo);
        self.pending_binding = None;
        self.reload_viewed()?;
        self.editing = Some(EditMode::Selecting { row });
        Ok(())
    }

    fn undo_last_edit(&mut self) -> Result<()> {
        let Some(undo) = self.last_undo.take() else {
            return Ok(());
        };
        if self.viewed_profile.as_deref() != Some(undo.profile_name.as_str()) {
            self.last_undo = Some(undo);
            return Ok(());
        }
        restore_binding(&undo, &self.active_profile)?;
        self.pending_binding = None;
        self.reload_viewed()?;
        Ok(())
    }

    fn handle_listen_key(&mut self, key: &KeyEvent, row: usize, prefix_seen: bool) -> Result<()> {
        let Some(config_key) = row_config_key(&self.rows[row]) else {
            self.editing = Some(EditMode::Selecting { row });
            return Ok(());
        };
        let is_prefix = config_key == "prefix";
        if let Some(pending) = self.pending_binding.clone() {
            match key.code {
                KeyCode::Enter => return self.save_edit(row, &pending),
                KeyCode::Backspace => {
                    self.pending_binding = None;
                    self.editing = Some(EditMode::Listening {
                        row,
                        prefix_seen: false,
                    });
                }
                KeyCode::Esc => {
                    self.pending_binding = None;
                    self.editing = Some(EditMode::Selecting { row });
                }
                _ => {}
            }
            return Ok(());
        }
        let Some(binding) = key_event_binding(key) else {
            return Ok(());
        };
        if is_prefix {
            self.pending_binding = Some(binding);
            return Ok(());
        }
        if key.code == KeyCode::Esc && !prefix_seen {
            let profile_name = self.viewed_profile.as_deref().unwrap_or_default();
            let prefix =
                profile_binding(profile_name, "prefix")?.unwrap_or_else(|| "ctrl+b".to_string());
            if !binding.eq_ignore_ascii_case(&prefix) {
                self.editing = Some(EditMode::Selecting { row });
                return Ok(());
            }
        }
        if key.code == KeyCode::Esc {
            self.editing = Some(EditMode::Selecting { row });
            return Ok(());
        }
        if prefix_seen {
            self.pending_binding = Some(format!("prefix+{binding}"));
            return Ok(());
        }
        let Some(profile_name) = self.viewed_profile.as_deref() else {
            return Ok(());
        };
        let prefix =
            profile_binding(profile_name, "prefix")?.unwrap_or_else(|| "ctrl+b".to_string());
        if binding.eq_ignore_ascii_case(&prefix) {
            self.editing = Some(EditMode::Listening {
                row,
                prefix_seen: true,
            });
            return Ok(());
        }
        self.pending_binding = Some(binding);
        Ok(())
    }
}

fn render(out: &mut impl Write, state: &mut ViewState) -> Result<()> {
    let (cols, term_rows) = terminal::size()?;
    let cols = cols as usize;

    let listening = matches!(state.editing, Some(EditMode::Listening { .. }));
    let prefix_listening = match state.editing {
        Some(EditMode::Listening { row, .. }) => row_config_key(&state.rows[row]) == Some("prefix"),
        _ => false,
    };
    let footer_lines = wrap_footer(
        &footer_segments(
            state.searching,
            state.editing.is_some(),
            listening,
            prefix_listening,
        ),
        cols,
    );
    // Fixed chrome: banner, tabs + blank, search/hint line + blank, trailing
    // blank, then the footer (which may itself wrap onto more than one line).
    let viewport = (term_rows as usize)
        .saturating_sub(6 + footer_lines.len())
        .max(1);
    let max_offset = state.filtered.len().saturating_sub(viewport);
    state.offset = state.offset.min(max_offset);

    let banner = format!(" active profile: {} ", state.active_profile);
    let pad = cols.saturating_sub(banner.chars().count()) / 2;

    let mut buf = String::new();
    let tabs = render_tabs(
        &state.profiles,
        state.viewed_profile.as_deref(),
        &state.active_profile,
        cols,
    );

    buf.push_str("\x1b[2J\x1b[H");
    buf.push_str(&" ".repeat(pad));
    buf.push_str(&format!(
        "{BOLD}{BG_CYAN}{FG_BLACK}{banner}{RESET}\x1b[K\r\n"
    ));
    buf.push_str(LEFT_PAD);
    buf.push_str(&tabs);
    buf.push_str("\x1b[K\r\n\r\n");
    if state.searching {
        buf.push_str(&format!(
            "{LEFT_PAD}{BOLD}/{RESET}{}\u{2588}\x1b[K\r\n\r\n",
            state.query
        ));
    } else if let Some(editing) = state.editing {
        let hint = if let Some(binding) = state.pending_binding.as_deref() {
            format!("captured {binding}; enter save; backspace retry; esc cancel")
        } else {
            match editing {
                EditMode::Selecting { .. } => {
                    "press enter to listen; esc to leave edit mode".to_string()
                }
                EditMode::Listening { row, .. }
                    if row_config_key(&state.rows[row]) == Some("prefix") =>
                {
                    "press the new prefix; ctrl+c to cancel".to_string()
                }
                EditMode::Listening {
                    prefix_seen: true, ..
                } => "prefix detected; press the next key; esc to cancel".to_string(),
                EditMode::Listening { .. } => {
                    "press direct key or this profile's prefix + key; terminal/Herdr may use it"
                        .to_string()
                }
            }
        };
        buf.push_str(&format!("{LEFT_PAD}{DIM}{hint}{RESET}\x1b[K\r\n\r\n"));
    } else {
        buf.push_str(&format!(
            "{LEFT_PAD}{DIM}press / to filter by command or shortcut{RESET}\x1b[K\r\n\r\n"
        ));
    }
    if state.filtered.is_empty() {
        buf.push_str(&format!("{LEFT_PAD}{DIM}no matches{RESET}\x1b[K\r\n"));
    }
    let selected_row = state.editing.map(|editing| match editing {
        EditMode::Selecting { row } | EditMode::Listening { row, .. } => row,
    });
    for &idx in state.filtered.iter().skip(state.offset).take(viewport) {
        buf.push_str(&render_row(
            &state.rows[idx],
            cols,
            selected_row == Some(idx),
        ));
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
                    MouseEventKind::ScrollUp => state.scroll(-3),
                    MouseEventKind::ScrollDown => state.scroll(3),
                    MouseEventKind::Down(MouseButton::Left)
                        if m.row == TABS_ROW && state.editing.is_none() =>
                    {
                        let cols = terminal::size()?.0 as usize;
                        if let Some(index) = tab_at_column(
                            &state.profiles,
                            state.viewed_profile.as_deref(),
                            &state.active_profile,
                            cols,
                            m.column,
                        ) {
                            state.select_profile(index)?;
                        }
                    }
                    _ => {}
                },
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                        match state.editing {
                            None => quit = true,
                            Some(EditMode::Selecting { .. }) => state.editing = None,
                            Some(EditMode::Listening { row, .. })
                                if row_config_key(&state.rows[row]) == Some("prefix") =>
                            {
                                state.editing = None;
                            }
                            Some(EditMode::Listening { row, prefix_seen }) => {
                                state.handle_listen_key(&k, row, prefix_seen)?;
                            }
                        }
                    } else if let Some(editing) = state.editing {
                        match editing {
                            EditMode::Selecting { .. } => match k.code {
                                KeyCode::Esc => state.editing = None,
                                KeyCode::Enter => state.listen_selected(),
                                KeyCode::Char('j') | KeyCode::Down => state.move_edit_selection(1),
                                KeyCode::Char('k') | KeyCode::Up => state.move_edit_selection(-1),
                                KeyCode::Char('u') => state.undo_last_edit()?,
                                _ => {}
                            },
                            EditMode::Listening { row, prefix_seen } => {
                                state.handle_listen_key(&k, row, prefix_seen)?;
                            }
                        }
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
                            KeyCode::Down => state.scroll(1),
                            KeyCode::Up => state.scroll(-1),
                            KeyCode::PageDown => state.scroll(PAGE_STEP as isize),
                            KeyCode::PageUp => state.scroll(-(PAGE_STEP as isize)),
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
                            KeyCode::Char('e') => state.begin_edit(),
                            KeyCode::Char('u') => state.undo_last_edit()?,
                            KeyCode::Char('/') => state.searching = true,
                            KeyCode::Char('j') | KeyCode::Down => state.scroll(1),
                            KeyCode::Char('k') | KeyCode::Up => state.scroll(-1),
                            KeyCode::Left => state.move_profile(-1)?,
                            KeyCode::Right => state.move_profile(1)?,
                            KeyCode::PageDown => state.scroll(PAGE_STEP as isize),
                            KeyCode::PageUp => state.scroll(-(PAGE_STEP as isize)),
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
