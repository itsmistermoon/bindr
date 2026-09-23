//! Replica of Herdr's built-in prefix+? keybinds panel. Profiles are shown as
//! tabs: selecting a tab only changes the profile being viewed; shift+K still
//! switches the active profile in Herdr without changing the selected tab.

use crate::{config, keybinds_data, keys, switch};
use anyhow::{Context, Result};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::{cursor, execute, terminal};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Write, stdout};
use std::time::Duration;
use toml_edit::{DocumentMut, Item, value};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const BG_CYAN: &str = "\x1b[46m";
const BG_GREY: &str = "\x1b[100m";
const BG_GREEN: &str = "\x1b[42m";
const FG_BLACK: &str = "\x1b[30m";
const RED: &str = "\x1b[31m";
const BG_RED: &str = "\x1b[41m";
const DEFAULT_PROFILE: &str = "default";
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

/// Return the row indices whose displayed shortcut occurs more than once.
/// Unset bindings are intentionally ignored: several Herdr actions are
/// unbound by default and those should not conflict with each other. Groups
/// made only of read-only custom commands are ignored because the editor
/// cannot resolve them.
fn duplicate_rows(rows: &[Row]) -> HashSet<usize> {
    duplicate_partners(rows).into_keys().collect()
}

/// Map each duplicated row to the other rows sharing its shortcut, so the
/// editor can name the exact conflict even when the partner is off screen.
fn duplicate_partners(rows: &[Row]) -> HashMap<usize, Vec<usize>> {
    let mut occurrences: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        let Row::Entry { key, .. } = row else {
            continue;
        };
        if key == "unset" {
            continue;
        }
        occurrences
            .entry(key.to_ascii_lowercase())
            .or_default()
            .push(index);
    }
    occurrences
        .into_values()
        .filter(|indices| {
            indices.len() > 1
                && indices
                    .iter()
                    .any(|&index| row_config_key(&rows[index]).is_some())
        })
        .flat_map(|indices| {
            indices
                .iter()
                .map(|&index| {
                    let others = indices.iter().copied().filter(|&o| o != index).collect();
                    (index, others)
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Human label for a conflicting row: its description, marked when it is a
/// read-only custom/plugin command.
fn conflict_label(row: &Row) -> String {
    match row {
        Row::Entry {
            config_key: None,
            description,
            ..
        } => format!("{description} (custom)"),
        Row::Entry { description, .. } => description.clone(),
        _ => String::new(),
    }
}

fn conflict_note(rows: &[Row], others: &[usize]) -> String {
    others
        .iter()
        .map(|&index| conflict_label(&rows[index]))
        .collect::<Vec<_>>()
        .join(", ")
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
    manual: bool,
    prefix_listening: bool,
    confirming: bool,
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
    } else if confirming {
        vec![
            FooterHint {
                label: "save",
                key: "y/enter",
            },
            FooterHint {
                label: "discard",
                key: "n",
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
    } else if manual {
        vec![
            FooterHint {
                label: "type",
                key: "text/backspace",
            },
            FooterHint {
                label: "clear",
                key: "ctrl+u",
            },
            FooterHint {
                label: "save",
                key: "enter",
            },
            FooterHint {
                label: "back",
                key: "esc",
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
                label: "manual",
                key: "m",
            },
            FooterHint {
                label: "next duplicate",
                key: "d",
            },
            FooterHint {
                label: "duplicates only",
                key: "f",
            },
            FooterHint {
                label: "scroll",
                key: "pgup/pgdn",
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
    Manual { row: usize },
    Confirming { row: usize },
}

#[derive(Clone, Copy)]
enum RowHighlight {
    Editing,
    Listening,
    Saved,
    Conflict,
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
    // Terminal events usually encode shifted punctuation in the character
    // itself (`?`, `!`, etc.). Herdr's syntax expects those characters as-is,
    // while shifted letters/digits still need the explicit shift modifier.
    let shifted_alphanumeric = key.modifiers.contains(KeyModifiers::SHIFT)
        && matches!(key.code, KeyCode::Char(c) if c.is_ascii_alphanumeric());
    if shifted_alphanumeric {
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

struct UndoEntry {
    profile_name: String,
    profile_previous: DocumentMut,
    live_previous: Option<DocumentMut>,
    active_profile: String,
}

fn save_undo(undo: &UndoEntry) -> Result<()> {
    let path = config::undo_file();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut doc = DocumentMut::new();
    doc["profile"] = value(&undo.profile_name);
    doc["active_profile"] = value(&undo.active_profile);
    doc["profile_previous"] = value(undo.profile_previous.to_string());
    doc["live_previous_present"] = value(undo.live_previous.is_some());
    if let Some(item) = &undo.live_previous {
        doc["live_previous"] = value(item.to_string());
    }
    fs::write(path, doc.to_string()).context("writing last keybind edit")?;
    Ok(())
}

fn load_undo() -> Result<Option<UndoEntry>> {
    let path = config::undo_file();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("reading last keybind edit"),
    };
    let doc = text
        .parse::<DocumentMut>()
        .context("parsing last keybind edit")?;
    let required = |key: &str| {
        doc.get(key)
            .and_then(Item::as_str)
            .ok_or_else(|| anyhow::anyhow!("last keybind edit is missing '{key}'"))
    };
    let document = |key: &str| -> Result<DocumentMut> {
        doc.get(key)
            .and_then(Item::as_str)
            .ok_or_else(|| anyhow::anyhow!("last keybind edit is missing '{key}'"))?
            .parse::<DocumentMut>()
            .with_context(|| format!("parsing {key} in last keybind edit"))
    };
    let live_previous = (doc.get("live_previous_present").and_then(Item::as_bool) == Some(true))
        .then(|| document("live_previous"))
        .transpose()?;
    Ok(Some(UndoEntry {
        profile_name: required("profile")?.to_string(),
        profile_previous: document("profile_previous")?,
        live_previous,
        active_profile: required("active_profile")?.to_string(),
    }))
}

fn clear_undo() -> Result<()> {
    match fs::remove_file(config::undo_file()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("removing last keybind edit"),
    }
}

fn commit_profile(
    profile_name: &str,
    active_profile: &str,
    profile: &DocumentMut,
) -> Result<UndoEntry> {
    let profile_path = config::profiles_dir().join(format!("{profile_name}.toml"));
    if profile_name == DEFAULT_PROFILE {
        anyhow::bail!("profile 'default' is read-only");
    }
    let profile_previous = keys::load(&profile_path)?;
    keys::save(&profile_path, profile)?;

    let live_previous = if profile_name == active_profile {
        let config_path = config::config_path();
        let mut live = keys::load(&config_path)?;
        let previous = live.clone();
        keys::apply_profile(&mut live, profile);
        keys::save(&config_path, &live)?;
        switch::reload_config()?;
        Some(previous)
    } else {
        None
    };
    Ok(UndoEntry {
        profile_name: profile_name.to_string(),
        profile_previous,
        live_previous,
        active_profile: active_profile.to_string(),
    })
}

fn restore_profile(undo: &UndoEntry, active_profile: &str) -> Result<()> {
    let profile_path = config::profiles_dir().join(format!("{}.toml", undo.profile_name));
    keys::save(&profile_path, &undo.profile_previous)?;

    if undo.profile_name == active_profile && undo.active_profile == active_profile {
        let config_path = config::config_path();
        if let Some(live_previous) = &undo.live_previous {
            keys::save(&config_path, live_previous)?;
            switch::reload_config()?;
        }
    }
    Ok(())
}

/// `conflict` carries the labels of the rows sharing this shortcut; it is shown
/// after the description so the partner is known without scrolling to it.
fn render_row(
    row: &Row,
    width: usize,
    highlight: Option<RowHighlight>,
    conflict: Option<&str>,
) -> String {
    match row {
        Row::Blank => String::new(),
        Row::Section(name) => format!("{LEFT_PAD}{BOLD}{CYAN}{name}{RESET}"),
        Row::Entry {
            key, description, ..
        } => {
            let description = &match conflict {
                Some(note) => format!("{description} \u{26a0} {note}"),
                None => description.clone(),
            };
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
            if let Some(highlight) = highlight {
                let (background, foreground) = match highlight {
                    RowHighlight::Editing => (BG_CYAN, FG_BLACK),
                    RowHighlight::Listening => (BG_GREY, FG_WHITE),
                    RowHighlight::Saved => (BG_GREEN, FG_BLACK),
                    RowHighlight::Conflict => (BG_RED, FG_WHITE),
                };
                format!(
                    "{background}{foreground}{LEFT_PAD}{BOLD}{key:<KEY_COL$}{RESET}{background}{foreground}{desc}{RESET}"
                )
            } else if conflict.is_some() {
                format!("{RED}{LEFT_PAD}{BOLD}{key:<KEY_COL$}{RESET}{RED}{desc}{RESET}")
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
    manual_binding: String,
    saved_row: Option<usize>,
    last_undo: Option<UndoEntry>,
    original_profile: Option<DocumentMut>,
    working_profile: Option<DocumentMut>,
    staged_undo: Option<(String, Option<Item>)>,
    /// Edit-mode quick filter: show only duplicated rows (plus the selection).
    duplicates_only: bool,
}

impl ViewState {
    fn load() -> Result<Self> {
        config::ensure_default_profile()?;
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
            manual_binding: String::new(),
            saved_row: None,
            // An unreadable undo file must not block the viewer; drop it.
            last_undo: match load_undo() {
                Ok(undo) => undo,
                Err(_) => {
                    clear_undo()?;
                    None
                }
            },
            original_profile: None,
            working_profile: None,
            staged_undo: None,
            duplicates_only: false,
        })
    }

    fn reload_viewed(&mut self) -> Result<()> {
        let offset = self.offset;
        let live = keys::load(&config::config_path())?;
        self.rows = match (&self.viewed_profile, &self.working_profile) {
            (Some(_), Some(profile)) => build_rows(profile, &live),
            (Some(name), None) => {
                let profile_path = config::profiles_dir().join(format!("{name}.toml"));
                let profile = keys::load(&profile_path)?;
                build_rows(&profile, &live)
            }
            (None, _) => build_rows_from_live_config()?,
        };
        self.filtered = self.visible_indices();
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
        self.filtered = self.visible_indices();
        self.offset = 0;
    }

    /// Rows matching the query, narrowed to duplicates (with their section
    /// headers) when the duplicates-only filter is on. The selected row stays
    /// visible after it is fixed so the selection never disappears.
    fn visible_indices(&self) -> Vec<usize> {
        let matching = filtered_indices(&self.rows, &self.query);
        if !self.duplicates_only {
            return matching;
        }
        let duplicates = duplicate_rows(&self.rows);
        let selected = self.editing.map(|editing| match editing {
            EditMode::Selecting { row }
            | EditMode::Listening { row, .. }
            | EditMode::Manual { row }
            | EditMode::Confirming { row } => row,
        });
        let mut result = Vec::new();
        let mut section: Option<usize> = None;
        for index in matching {
            match self.rows[index] {
                Row::Section(_) => section = Some(index),
                Row::Blank => {}
                Row::Entry { .. } => {
                    if duplicates.contains(&index) || selected == Some(index) {
                        if let Some(header) = section.take() {
                            result.push(header);
                        }
                        result.push(index);
                    }
                }
            }
        }
        result
    }

    fn toggle_duplicates_only(&mut self) {
        self.duplicates_only = !self.duplicates_only;
        self.refilter();
        if let Some(EditMode::Selecting { row }) = self.editing {
            self.keep_edit_row_visible(row);
        }
    }

    /// Move the selection to the next editable duplicated row, wrapping.
    fn next_duplicate(&mut self) {
        let Some(EditMode::Selecting { row }) = self.editing else {
            return;
        };
        let duplicates = duplicate_rows(&self.rows);
        let candidates: Vec<usize> = self
            .filtered
            .iter()
            .copied()
            .filter(|index| {
                duplicates.contains(index) && row_config_key(&self.rows[*index]).is_some()
            })
            .collect();
        let Some(&next) = candidates
            .iter()
            .find(|&&index| index > row)
            .or_else(|| candidates.first())
        else {
            return;
        };
        self.saved_row = None;
        self.editing = Some(EditMode::Selecting { row: next });
        self.keep_edit_row_visible(next);
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
        if self.viewed_profile.as_deref() != Some(DEFAULT_PROFILE) {
            let Some(profile_name) = self.viewed_profile.as_deref() else {
                return;
            };
            let profile_path = config::profiles_dir().join(format!("{profile_name}.toml"));
            let Ok(profile) = keys::load(&profile_path) else {
                return;
            };
            self.searching = false;
            self.query.clear();
            self.duplicates_only = false;
            self.refilter();
            self.saved_row = None;
            self.original_profile = Some(profile.clone());
            self.working_profile = Some(profile);
            self.staged_undo = None;
            if self.reload_viewed().is_err() {
                self.original_profile = None;
                self.working_profile = None;
                return;
            }
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
        let footer_lines = wrap_footer(
            &footer_segments(false, true, false, false, false, false),
            cols as usize,
        )
        .len();
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
        // Past the first/last editable row, keep scrolling so read-only rows
        // (such as the trailing custom section) can still be reviewed.
        if next == current {
            self.scroll(delta);
            return;
        }
        let row = editable[next];
        self.saved_row = None;
        self.editing = Some(EditMode::Selecting { row });
        self.keep_edit_row_visible(row);
    }

    fn listen_selected(&mut self) {
        if let Some(EditMode::Selecting { row }) = self.editing {
            self.saved_row = None;
            self.pending_binding = None;
            self.editing = Some(EditMode::Listening {
                row,
                prefix_seen: false,
            });
        }
    }

    fn begin_manual_edit(&mut self) -> Result<()> {
        let Some(EditMode::Selecting { row }) = self.editing else {
            return Ok(());
        };
        let Some(config_key) = row_config_key(&self.rows[row]) else {
            return Ok(());
        };
        let Some(profile_name) = self.viewed_profile.as_deref() else {
            return Ok(());
        };
        self.saved_row = None;
        self.manual_binding = self
            .current_profile_binding(profile_name, config_key)?
            .unwrap_or_default();
        self.pending_binding = None;
        self.editing = Some(EditMode::Manual { row });
        Ok(())
    }

    fn save_edit(&mut self, row: usize, binding: &str) -> Result<()> {
        let Some(config_key) = row_config_key(&self.rows[row]) else {
            return Ok(());
        };
        let Some(profile) = self.working_profile.as_mut() else {
            return Ok(());
        };
        let previous = keys::get_item(profile, config_key);
        keys::set_scalar(profile, config_key, binding);
        self.staged_undo = Some((config_key.to_string(), previous));
        self.saved_row = Some(row);
        self.pending_binding = None;
        self.manual_binding.clear();
        self.reload_viewed()?;
        self.editing = Some(EditMode::Selecting { row });
        Ok(())
    }

    fn current_profile_binding(
        &self,
        profile_name: &str,
        config_key: &str,
    ) -> Result<Option<String>> {
        let profile = match self.working_profile.as_ref() {
            Some(profile) => profile.clone(),
            None => {
                let path = config::profiles_dir().join(format!("{profile_name}.toml"));
                keys::load(&path)?
            }
        };
        let value = keys::scalar_overrides(&profile)
            .into_iter()
            .find(|(key, _)| key == config_key)
            .map(|(_, value)| value)
            .or_else(|| default_binding(config_key).map(str::to_string));
        Ok(value)
    }

    fn has_unresolved_duplicates(&self) -> bool {
        !duplicate_rows(&self.rows).is_empty()
    }

    fn has_pending_changes(&self) -> bool {
        match (&self.original_profile, &self.working_profile) {
            (Some(original), Some(working)) => original.to_string() != working.to_string(),
            _ => false,
        }
    }

    fn request_leave_edit(&mut self) {
        let Some(EditMode::Selecting { row }) = self.editing else {
            return;
        };
        if self.has_pending_changes() {
            self.editing = Some(EditMode::Confirming { row });
        } else {
            self.discard_edit();
        }
    }

    fn discard_edit(&mut self) {
        self.duplicates_only = false;
        self.original_profile = None;
        self.working_profile = None;
        self.staged_undo = None;
        self.pending_binding = None;
        self.manual_binding.clear();
        self.editing = None;
        self.saved_row = None;
        let _ = self.reload_viewed();
    }

    fn confirm_save(&mut self) -> Result<()> {
        let Some(profile_name) = self.viewed_profile.clone() else {
            return Ok(());
        };
        let Some(profile) = self.working_profile.as_ref() else {
            return Ok(());
        };
        if self.has_unresolved_duplicates() {
            self.editing = self.editing.map(|editing| match editing {
                EditMode::Confirming { row } | EditMode::Selecting { row } => {
                    EditMode::Selecting { row }
                }
                other => other,
            });
            return Ok(());
        }
        let undo = commit_profile(&profile_name, &self.active_profile, profile)?;
        save_undo(&undo)?;
        self.last_undo = Some(undo);
        self.original_profile = None;
        self.working_profile = None;
        self.duplicates_only = false;
        self.staged_undo = None;
        self.editing = None;
        self.pending_binding = None;
        self.manual_binding.clear();
        self.reload_viewed()
    }

    fn undo_last_edit(&mut self) -> Result<()> {
        if let (Some(profile), Some((config_key, previous))) =
            (self.working_profile.as_mut(), self.staged_undo.take())
        {
            match previous {
                Some(item) => keys::set_item(profile, &config_key, item),
                None => keys::remove_scalar(profile, &config_key),
            }
            self.saved_row = None;
            self.reload_viewed()?;
            return Ok(());
        }
        let Some(undo) = self.last_undo.take() else {
            return Ok(());
        };
        if self.viewed_profile.as_deref() != Some(undo.profile_name.as_str()) {
            self.viewed_profile = Some(undo.profile_name.clone());
        }
        if let Err(error) = restore_profile(&undo, &self.active_profile) {
            self.last_undo = Some(undo);
            return Err(error);
        }
        clear_undo()?;
        self.saved_row = None;
        self.pending_binding = None;
        self.manual_binding.clear();
        self.reload_viewed()?;
        Ok(())
    }

    fn handle_manual_key(&mut self, key: &KeyEvent, row: usize) -> Result<()> {
        if key.code == KeyCode::Char('u') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.manual_binding.clear();
            return Ok(());
        }
        match key.code {
            KeyCode::Enter => {
                let binding = self.manual_binding.clone();
                self.save_edit(row, &binding)
            }
            KeyCode::Esc => {
                self.manual_binding.clear();
                self.editing = Some(EditMode::Selecting { row });
                Ok(())
            }
            KeyCode::Backspace => {
                self.manual_binding.pop();
                Ok(())
            }
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.manual_binding.push(c);
                Ok(())
            }
            _ => Ok(()),
        }
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
            let prefix = self
                .current_profile_binding(profile_name, "prefix")?
                .unwrap_or_else(|| "ctrl+b".to_string());
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
        let prefix = self
            .current_profile_binding(profile_name, "prefix")?
            .unwrap_or_else(|| "ctrl+b".to_string());
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

/// Truncate `text` to `width` columns with an ellipsis. The hint line must never
/// wrap: an extra line would push the output past the popup height and scroll
/// the banner off screen.
fn fit_line(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('\u{2026}');
    cut
}

/// Rows sharing the shortcut of the row selected in edit mode, if any.
fn selected_conflict(state: &ViewState) -> Option<Vec<usize>> {
    let Some(EditMode::Selecting { row }) = state.editing else {
        return None;
    };
    duplicate_partners(&state.rows).remove(&row)
}

fn render(out: &mut impl Write, state: &mut ViewState) -> Result<()> {
    let (cols, term_rows) = terminal::size()?;
    let cols = cols as usize;

    let listening = matches!(state.editing, Some(EditMode::Listening { .. }));
    let manual = matches!(state.editing, Some(EditMode::Manual { .. }));
    let confirming = matches!(state.editing, Some(EditMode::Confirming { .. }));
    let prefix_listening = match state.editing {
        Some(EditMode::Listening { row, .. }) => row_config_key(&state.rows[row]) == Some("prefix"),
        _ => false,
    };
    let footer_lines = wrap_footer(
        &footer_segments(
            state.searching,
            state.editing.is_some(),
            listening,
            manual,
            prefix_listening,
            confirming,
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
        let hint = if confirming && state.has_unresolved_duplicates() {
            "resolve red duplicates to save; n discard; esc back".to_string()
        } else if confirming {
            "save changes to this keybind profile? y/enter yes; n discard; esc back".to_string()
        } else if let Some(binding) = state.pending_binding.as_deref() {
            format!("captured {binding}; enter save; backspace retry; esc cancel")
        } else if let Some(others) = selected_conflict(state) {
            format!(
                "also used by {}; d next duplicate; f duplicates only",
                conflict_note(&state.rows, &others)
            )
        } else if state.has_unresolved_duplicates() {
            "duplicates are red; resolve them to save, or esc to discard".to_string()
        } else {
            match editing {
                EditMode::Selecting { .. } => {
                    "press enter to listen; m for manual text; esc to leave edit mode".to_string()
                }
                EditMode::Manual { .. } => format!(
                    "binding: {}\u{2588}; ctrl+u clear; enter save; esc cancel",
                    state.manual_binding
                ),
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
                EditMode::Confirming { .. } => unreachable!(),
            }
        };
        buf.push_str(&format!(
            "{LEFT_PAD}{DIM}{}{RESET}\x1b[K\r\n\r\n",
            fit_line(&hint, cols.saturating_sub(LEFT_PAD.len()))
        ));
    } else {
        let hint = if state.viewed_profile.as_deref() == Some(DEFAULT_PROFILE) {
            "default profile is read-only; press / to filter by command or shortcut"
        } else {
            "press / to filter by command or shortcut"
        };
        buf.push_str(&format!(
            "{LEFT_PAD}{DIM}{}{RESET}\x1b[K\r\n\r\n",
            fit_line(hint, cols.saturating_sub(LEFT_PAD.len()))
        ));
    }
    if state.filtered.is_empty() {
        buf.push_str(&format!("{LEFT_PAD}{DIM}no matches{RESET}\x1b[K\r\n"));
    }
    let conflicts = if state.editing.is_some() {
        duplicate_partners(&state.rows)
    } else {
        HashMap::new()
    };
    let selected_row = state.editing.map(|editing| match editing {
        EditMode::Selecting { row } => (
            row,
            if conflicts.contains_key(&row) {
                RowHighlight::Conflict
            } else if state.saved_row == Some(row) {
                RowHighlight::Saved
            } else {
                RowHighlight::Editing
            },
        ),
        EditMode::Listening { row, .. } => (row, RowHighlight::Listening),
        EditMode::Manual { row } => (row, RowHighlight::Listening),
        EditMode::Confirming { row } => (row, RowHighlight::Editing),
    });
    for &idx in state.filtered.iter().skip(state.offset).take(viewport) {
        buf.push_str(&render_row(
            &state.rows[idx],
            cols,
            selected_row
                .filter(|(row, _)| *row == idx)
                .map(|(_, highlight)| highlight),
            conflicts
                .get(&idx)
                .map(|others| conflict_note(&state.rows, others))
                .as_deref(),
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
                            Some(EditMode::Selecting { .. }) => state.request_leave_edit(),
                            Some(EditMode::Listening { row, .. })
                                if row_config_key(&state.rows[row]) == Some("prefix") =>
                            {
                                state.editing = Some(EditMode::Selecting { row });
                            }
                            Some(EditMode::Listening { row, prefix_seen }) => {
                                state.handle_listen_key(&k, row, prefix_seen)?;
                            }
                            Some(EditMode::Manual { row }) => {
                                state.editing = Some(EditMode::Selecting { row });
                            }
                            Some(EditMode::Confirming { .. }) => {}
                        }
                    } else if let Some(editing) = state.editing {
                        match editing {
                            EditMode::Selecting { .. } => match k.code {
                                KeyCode::Esc => state.request_leave_edit(),
                                KeyCode::Enter => state.listen_selected(),
                                KeyCode::Char('j') | KeyCode::Down => state.move_edit_selection(1),
                                KeyCode::Char('k') | KeyCode::Up => state.move_edit_selection(-1),
                                KeyCode::Char('m') => state.begin_manual_edit()?,
                                KeyCode::Char('u') => state.undo_last_edit()?,
                                KeyCode::Char('d') => state.next_duplicate(),
                                KeyCode::Char('f') => state.toggle_duplicates_only(),
                                KeyCode::PageDown => state.scroll(PAGE_STEP as isize),
                                KeyCode::PageUp => state.scroll(-(PAGE_STEP as isize)),
                                _ => {}
                            },
                            EditMode::Listening { row, prefix_seen } => {
                                state.handle_listen_key(&k, row, prefix_seen)?;
                            }
                            EditMode::Manual { row } => state.handle_manual_key(&k, row)?,
                            EditMode::Confirming { .. } => match k.code {
                                KeyCode::Char('y') | KeyCode::Enter => state.confirm_save()?,
                                KeyCode::Char('n') => state.discard_edit(),
                                KeyCode::Esc => {
                                    if let Some(EditMode::Confirming { row }) = state.editing {
                                        state.editing = Some(EditMode::Selecting { row });
                                    }
                                }
                                _ => {}
                            },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_keys_use_a_remote_safe_emphasis() {
        let lines = wrap_footer(
            &footer_segments(false, false, false, false, false, false),
            76,
        );
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

    #[test]
    fn shifted_punctuation_is_not_saved_with_a_redundant_shift_modifier() {
        let key = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert_eq!(key_event_binding(&key).as_deref(), Some("?"));
    }

    #[test]
    fn edit_row_highlight_uses_the_mode_color() {
        let row = Row::Entry {
            config_key: Some("new_tab".to_string()),
            key: "prefix+c".to_string(),
            description: "open a tab".to_string(),
        };

        let listening = render_row(&row, 76, Some(RowHighlight::Listening), None);
        assert!(listening.contains(BG_GREY));
        assert!(listening.contains(FG_WHITE));

        let saved = render_row(&row, 76, Some(RowHighlight::Saved), None);
        assert!(saved.contains(BG_GREEN));
        assert!(saved.contains(FG_BLACK));
    }

    #[test]
    fn duplicate_rows_include_both_bindings_and_ignore_unset_rows() {
        let rows = vec![
            Row::Entry {
                config_key: Some("one".to_string()),
                key: "prefix+x".to_string(),
                description: "one".to_string(),
            },
            Row::Entry {
                config_key: Some("two".to_string()),
                key: "PREFIX+X".to_string(),
                description: "two".to_string(),
            },
            Row::Entry {
                config_key: None,
                key: "unset".to_string(),
                description: "three".to_string(),
            },
        ];
        let conflicts = duplicate_rows(&rows);
        assert_eq!(conflicts, HashSet::from([0, 1]));
    }

    #[test]
    fn duplicate_row_text_and_selector_use_ansi_red() {
        let row = Row::Entry {
            config_key: Some("one".to_string()),
            key: "prefix+x".to_string(),
            description: "one".to_string(),
        };
        let text = render_row(&row, 76, None, Some("two"));
        let selector = render_row(&row, 76, Some(RowHighlight::Conflict), Some("two"));
        assert!(text.contains(RED));
        assert!(selector.contains(BG_RED));
    }

    fn test_state(rows: Vec<Row>, dirty: bool) -> ViewState {
        let original: DocumentMut = "[keys]\none = \"prefix+a\"\n".parse().unwrap();
        let working: DocumentMut = if dirty {
            "[keys]\none = \"prefix+b\"\n".parse().unwrap()
        } else {
            original.clone()
        };
        ViewState {
            profiles: vec!["work".to_string()],
            viewed_profile: Some("work".to_string()),
            filtered: (0..rows.len()).collect(),
            rows,
            active_profile: "work".to_string(),
            offset: 0,
            query: String::new(),
            searching: false,
            editing: Some(EditMode::Selecting { row: 0 }),
            pending_binding: None,
            manual_binding: String::new(),
            saved_row: None,
            last_undo: None,
            original_profile: Some(original),
            working_profile: Some(working),
            staged_undo: None,
            duplicates_only: false,
        }
    }

    #[test]
    fn unresolved_duplicate_blocks_saving_but_not_leaving() {
        let rows = vec![
            Row::Entry {
                config_key: Some("one".to_string()),
                key: "prefix+x".to_string(),
                description: "one".to_string(),
            },
            Row::Entry {
                config_key: Some("two".to_string()),
                key: "prefix+x".to_string(),
                description: "two".to_string(),
            },
        ];
        let mut state = test_state(rows, true);
        state.request_leave_edit();
        assert!(matches!(state.editing, Some(EditMode::Confirming { .. })));
        state.confirm_save().unwrap();
        assert!(matches!(state.editing, Some(EditMode::Selecting { .. })));
    }

    #[test]
    fn pending_changes_require_save_confirmation_before_leaving() {
        let rows = vec![Row::Entry {
            config_key: Some("one".to_string()),
            key: "prefix+b".to_string(),
            description: "one".to_string(),
        }];
        let mut state = test_state(rows, true);
        state.request_leave_edit();
        assert!(matches!(state.editing, Some(EditMode::Confirming { .. })));
    }

    fn conflict_rows() -> Vec<Row> {
        vec![
            Row::Section("panes".to_string()),
            Row::Entry {
                config_key: Some("focus_pane_up".to_string()),
                key: "prefix+k".to_string(),
                description: "focus pane up".to_string(),
            },
            Row::Entry {
                config_key: Some("other".to_string()),
                key: "prefix+o".to_string(),
                description: "other".to_string(),
            },
            Row::Section("custom".to_string()),
            Row::Entry {
                config_key: None,
                key: "prefix+k".to_string(),
                description: "command bar".to_string(),
            },
        ]
    }

    #[test]
    fn conflict_note_names_the_partner() {
        let rows = conflict_rows();
        let partners = duplicate_partners(&rows);
        assert_eq!(conflict_note(&rows, &partners[&1]), "command bar (custom)");
        assert_eq!(conflict_note(&rows, &partners[&4]), "focus pane up");
    }

    #[test]
    fn duplicates_only_filter_keeps_conflicts_with_headers() {
        let mut state = test_state(conflict_rows(), false);
        state.editing = Some(EditMode::Selecting { row: 1 });
        state.toggle_duplicates_only();
        assert_eq!(state.filtered, vec![0, 1, 3, 4]);
        state.toggle_duplicates_only();
        assert_eq!(state.filtered, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn next_duplicate_selects_editable_conflict() {
        let mut state = test_state(conflict_rows(), false);
        state.editing = Some(EditMode::Selecting { row: 2 });
        state.next_duplicate();
        assert!(matches!(
            state.editing,
            Some(EditMode::Selecting { row: 1 })
        ));
    }
}
