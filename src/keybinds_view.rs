//! Replica of Herdr's built-in prefix+? keybinds panel. Profiles are shown as
//! tabs: selecting a tab only changes the profile being viewed; shift+K still
//! switches the active profile in Herdr without changing the selected tab.

use crate::keys::Target;
use crate::{config, keybinds_data, keys, save, switch};
use anyhow::{Context, Result};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::{cursor, execute, terminal};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Write, stdout};
use std::time::{Duration, Instant};
use toml_edit::{DocumentMut, Item, value};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const BLUE: &str = "\x1b[94m";
const BORDER: &str = "\x1b[2;94m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[92m";
const PILL_GREEN: &str = "\x1b[32m";
const PURPLE: &str = "\x1b[95m";
const DIM: &str = "\x1b[2m";
const BG_CYAN: &str = "\x1b[46m";
const BG_GREEN: &str = "\x1b[42m";
const BG_SAVED: &str = "\x1b[105m";
const FG_FOCUS_BLUE: &str = "\x1b[38;2;105;163;255m";
const BG_GREY: &str = "\x1b[100m";
const FG_BLACK: &str = "\x1b[30m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const BG_RED: &str = "\x1b[41m";
const DEFAULT_PROFILE: &str = "default";
// Use standard white plus bold instead of bright-white (97) alone. The
// latter can collapse to the normal foreground in remote/limited-color PTYs.
const FG_WHITE: &str = "\x1b[1;37m";
#[cfg(test)]
const KEY_COL: usize = 30;
const LEFT_PAD: &str = "  ";
const PAGE_STEP: usize = 10;
const TABS_ROW: u16 = 2;
const SEARCH_INPUT_ROW: u16 = 5;

enum Row {
    Section(String),
    Blank,
    Entry {
        /// What editing this row changes; `None` for read-only rows.
        target: Option<Target>,
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

/// `displaced` holds plugin bindings currently replaced by a live profile
/// override, so a profile without its own override shows the plugin's
/// binding rather than whichever profile is active.
fn build_rows(
    profile_doc: &toml_edit::DocumentMut,
    custom_doc: &toml_edit::DocumentMut,
    displaced: &toml_edit::DocumentMut,
) -> Vec<Row> {
    let overrides: HashMap<String, String> =
        keys::binding_overrides(profile_doc).into_iter().collect();
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
                target: r.config_key.map(|k| Target::Key(k.to_string())),
                key: display_value(value),
                description: r.description.to_string(),
            });
        }
        rows.push(Row::Blank);
    }

    rows.push(Row::Section("custom".to_string()));
    for command in custom {
        let key = command
            .id
            .as_deref()
            .and_then(|id| {
                keys::plugin_binding(profile_doc, id)
                    .or_else(|| displaced.get(id).and_then(keys::binding_text))
            })
            .unwrap_or_else(|| keys::binding_text(&command.key).unwrap_or_default());
        rows.push(Row::Entry {
            target: command.id.map(Target::Command),
            key: display_value(&key),
            description: command.description,
        });
    }
    rows
}

fn build_rows_from_live_config() -> Result<Vec<Row>> {
    let doc = keys::load(&config::config_path())?;
    Ok(build_rows(&doc, &doc, &DocumentMut::new()))
}

fn build_rows_for_profile(name: &str) -> Result<Vec<Row>> {
    let profile_path = config::profiles_dir().join(format!("{name}.toml"));
    let profile = keys::load(&profile_path)?;
    let live = keys::load(&config::config_path())?;
    Ok(build_rows(
        &profile,
        &live,
        &config::load_displaced_plugin_keys()?,
    ))
}

/// Return the row indices whose displayed shortcut occurs more than once.
/// Unset bindings are intentionally ignored: several Herdr actions are
/// unbound by default and those should not conflict with each other. Groups
/// made only of read-only rows are ignored because the editor cannot resolve
/// them.
fn duplicate_rows(rows: &[Row]) -> HashSet<usize> {
    duplicate_partners(rows).into_keys().collect()
}

/// Map each duplicated row to the other rows sharing its shortcut, so the
/// editor can name the exact conflict even when the partner is off screen.
fn duplicate_partners(rows: &[Row]) -> HashMap<usize, Vec<usize>> {
    let mut occurrences: HashMap<(bool, String), Vec<usize>> = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        let Row::Entry { key, .. } = row else {
            continue;
        };
        if key == "unset" {
            continue;
        }
        let bindings = key.split(", ");
        let in_navigation = row_config_key(row).is_some_and(|key| key.starts_with("navigate_"));
        for binding in bindings {
            occurrences
                .entry((in_navigation, binding.to_ascii_lowercase()))
                .or_default()
                .push(index);
        }
    }
    let mut partners: HashMap<usize, HashSet<usize>> = HashMap::new();
    for indices in occurrences.into_values() {
        if indices.len() < 2
            || !indices
                .iter()
                .any(|&index| row_target(&rows[index]).is_some())
        {
            continue;
        }
        for &index in &indices {
            partners
                .entry(index)
                .or_default()
                .extend(indices.iter().copied().filter(|&other| other != index));
        }
    }
    partners
        .into_iter()
        .map(|(index, others)| {
            let mut others: Vec<usize> = others.into_iter().collect();
            others.sort_unstable();
            (index, others)
        })
        .collect()
}

/// Human label for a conflicting row: its description, marked when it is a
/// custom/plugin command.
fn conflict_label(row: &Row) -> String {
    match row {
        Row::Entry {
            target: None | Some(Target::Command(_)),
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

fn naming_footer_lines(cols: usize) -> Vec<String> {
    wrap_footer(
        &[
            FooterHint {
                label: "name",
                key: "a-z/0-9/-/_",
            },
            FooterHint {
                label: "create",
                key: "enter",
            },
            FooterHint {
                label: "cancel",
                key: "esc",
            },
        ],
        cols,
    )
}

const FOOTER_SEP: &str = " \u{b7} ";

fn grouped_footer_lines(groups: &[(&str, &[FooterHint])], cols: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for &(category, hints) in groups {
        let category_width = category.chars().count() + 1;
        let available = cols.saturating_sub(category_width);
        let wrapped = wrap_footer(hints, available);
        for (index, line) in wrapped.into_iter().enumerate() {
            if index == 0 {
                lines.push(format!("{BOLD}{PURPLE}{category}{RESET} {line}"));
            } else {
                lines.push(format!("{}{line}", " ".repeat(category_width)));
            }
        }
    }
    lines
}

fn browse_footer_lines(cols: usize) -> Vec<String> {
    let groups: [(&str, &[FooterHint]); 3] = [
        (
            "NAVIGATION",
            &[
                FooterHint {
                    label: "tabs",
                    key: "\u{2190}/\u{2192}/click",
                },
                FooterHint {
                    label: "scroll",
                    key: "j/k/\u{2191}\u{2193}/pgup/pgdn",
                },
            ],
        ),
        (
            "PROFILE",
            &[
                FooterHint {
                    label: "switch",
                    key: "shift+k",
                },
                FooterHint {
                    label: "rename",
                    key: "r",
                },
                FooterHint {
                    label: "new",
                    key: "shift+n",
                },
                FooterHint {
                    label: "delete",
                    key: "shift+d",
                },
            ],
        ),
        (
            "ACTIONS",
            &[
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
                    label: "close",
                    key: "esc/enter/q",
                },
            ],
        ),
    ];

    grouped_footer_lines(&groups, cols)
}

fn filter_footer_lines(cols: usize) -> Vec<String> {
    let filter = [
        FooterHint {
            label: "filter",
            key: "type/backspace",
        },
        FooterHint {
            label: "clear",
            key: "ctrl+u",
        },
    ];
    let navigation = [
        FooterHint {
            label: "scroll",
            key: "\u{2191}\u{2193}/pgup/pgdn",
        },
        FooterHint {
            label: "back",
            key: "esc",
        },
    ];
    grouped_footer_lines(&[("FILTER", &filter), ("NAVIGATION", &navigation)], cols)
}

fn rename_footer_lines(cols: usize) -> Vec<String> {
    let input = [FooterHint {
        label: "new name",
        key: "a-z/0-9/-/_",
    }];
    let actions = [
        FooterHint {
            label: "rename",
            key: "enter",
        },
        FooterHint {
            label: "cancel",
            key: "esc",
        },
    ];
    grouped_footer_lines(&[("INPUT", &input), ("ACTIONS", &actions)], cols)
}

fn delete_footer_lines(cols: usize) -> Vec<String> {
    let actions = [
        FooterHint {
            label: "delete profile",
            key: "y/enter",
        },
        FooterHint {
            label: "cancel",
            key: "n/esc",
        },
    ];
    grouped_footer_lines(&[("ACTIONS", &actions)], cols)
}

fn edit_footer_lines(mode: EditMode, cols: usize, prefix_listening: bool) -> Vec<String> {
    let navigation = [
        FooterHint {
            label: "select",
            key: "j/k/\u{2191}\u{2193}",
        },
        FooterHint {
            label: "scroll",
            key: "pgup/pgdn",
        },
        FooterHint {
            label: "back",
            key: "esc",
        },
    ];
    let binding = [
        FooterHint {
            label: "listen",
            key: "enter",
        },
        FooterHint {
            label: "manual",
            key: "m",
        },
        FooterHint {
            label: "unset",
            key: "x",
        },
    ];
    let duplicates = [
        FooterHint {
            label: "next duplicate",
            key: "d",
        },
        FooterHint {
            label: "duplicates only",
            key: "f",
        },
    ];
    let capture = [
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
    ];
    let input = [
        FooterHint {
            label: "type",
            key: "text/backspace",
        },
        FooterHint {
            label: "clear",
            key: "ctrl+u",
        },
    ];
    let save = [
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
    ];
    let manual_actions = [
        FooterHint {
            label: "save",
            key: "enter",
        },
        FooterHint {
            label: "cancel",
            key: "esc",
        },
    ];

    match mode {
        EditMode::Selecting { .. } => grouped_footer_lines(
            &[
                ("NAVIGATION", &navigation),
                ("BINDING", &binding),
                ("DUPLICATES", &duplicates),
            ],
            cols,
        ),
        EditMode::Listening { .. } => grouped_footer_lines(&[("CAPTURE", &capture)], cols),
        EditMode::Manual { .. } => {
            grouped_footer_lines(&[("INPUT", &input), ("ACTIONS", &manual_actions)], cols)
        }
        EditMode::Confirming { .. } => grouped_footer_lines(&[("SAVE", &save)], cols),
    }
}

fn tab_width(name: &str) -> usize {
    name.chars().count() + 6
}

fn styled_tab(name: &str, selected: bool, active: bool) -> String {
    if selected {
        format!("{PILL_GREEN}{BG_GREEN}{FG_BLACK}{BOLD} [{name}] {RESET}{PILL_GREEN}{RESET}")
    } else if active {
        format!("{BOLD}{GREEN}   {name}   {RESET}")
    } else {
        format!("{DIM}   {name}   {RESET}")
    }
}

/// Keep the first tab pinned and return the visible range of the remaining
/// tabs. The viewed profile stays visible when the strip overflows.
fn visible_tab_range(profiles: &[String], viewed: Option<&str>, cols: usize) -> (usize, usize) {
    if profiles.len() <= 1 {
        return (1, 1);
    }

    let selected = viewed
        .and_then(|name| profiles.iter().position(|profile| profile == name))
        .unwrap_or(0);
    let budget = cols.saturating_sub(LEFT_PAD.chars().count());
    let mut start = 1;
    let mut end = profiles.len();

    let strip_width = |start: usize, end: usize| {
        let pinned_width = tab_width(&profiles[0]);
        if start == end {
            return pinned_width + 2;
        }
        let tabs_width: usize = profiles[start..end]
            .iter()
            .map(|name| tab_width(name))
            .sum();
        let separators = end - start - 1;
        let leading = if start > 1 { 3 } else { 1 };
        let trailing = if end < profiles.len() { 2 } else { 0 };
        pinned_width + leading + tabs_width + separators + trailing
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
    cols: usize,
    column: u16,
) -> Option<usize> {
    let pinned = profiles.first()?;
    let mut x = LEFT_PAD.chars().count();
    let pinned_width = tab_width(pinned);
    if usize::from(column) >= x && usize::from(column) < x + pinned_width {
        return Some(0);
    }
    let (start, end) = visible_tab_range(profiles, viewed, cols);
    if start == end {
        return None;
    }
    x += pinned_width + if start > 1 { 3 } else { 1 };
    for (i, name) in profiles[start..end].iter().enumerate() {
        let index = start + i;
        let width = tab_width(name);
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
    let (start, end) = visible_tab_range(profiles, viewed, cols);

    let mut line = styled_tab(&profiles[0], selected == 0, profiles[0] == active);
    if start == end {
        if profiles.len() > 1 {
            line.push_str(&format!(" {DIM}\u{2026}{RESET}"));
        }
        return line;
    }
    if start > 1 {
        line.push_str(&format!(" {DIM}\u{2026}{RESET} "));
    } else {
        line.push(' ');
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

fn row_target(row: &Row) -> Option<&Target> {
    match row {
        Row::Entry { target, .. } => target.as_ref(),
        _ => None,
    }
}

/// The built-in `[keys]` name of a row, if it is one (e.g. `prefix`).
fn row_config_key(row: &Row) -> Option<&str> {
    match row_target(row) {
        Some(Target::Key(key)) => Some(key),
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

/// Crossterm terminals may report a shifted letter as either an uppercase
/// character or a lowercase character with the SHIFT modifier set.
fn shifted_char(key: &KeyEvent, uppercase: char) -> bool {
    key.code == KeyCode::Char(uppercase)
        || (key.code == KeyCode::Char(uppercase.to_ascii_lowercase())
            && key.modifiers.contains(KeyModifiers::SHIFT))
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
    renamed_to: Option<String>,
    profile_previous: DocumentMut,
    live_previous: Option<DocumentMut>,
    active_profile: String,
    active_profile_after: String,
}

fn save_undo(undo: &UndoEntry) -> Result<()> {
    let path = config::undo_file();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut doc = DocumentMut::new();
    doc["profile"] = value(&undo.profile_name);
    doc["renamed_to"] = value(undo.renamed_to.as_deref().unwrap_or_default());
    doc["active_profile"] = value(&undo.active_profile);
    doc["active_profile_after"] = value(&undo.active_profile_after);
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
        renamed_to: match required("renamed_to")? {
            "" => None,
            name => Some(name.to_string()),
        },
        profile_previous: document("profile_previous")?,
        live_previous,
        active_profile: required("active_profile")?.to_string(),
        active_profile_after: required("active_profile_after")?.to_string(),
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
        let previous = switch::apply_to_config(profile)?;
        switch::reload_config()?;
        Some(previous)
    } else {
        None
    };
    Ok(UndoEntry {
        profile_name: profile_name.to_string(),
        renamed_to: None,
        profile_previous,
        live_previous,
        active_profile: active_profile.to_string(),
        active_profile_after: active_profile.to_string(),
    })
}

fn restore_profile(undo: &UndoEntry, active_profile: &str) -> Result<()> {
    if let Some(renamed_to) = &undo.renamed_to {
        let renamed_path = config::profiles_dir().join(format!("{renamed_to}.toml"));
        let original_path = config::profiles_dir().join(format!("{}.toml", undo.profile_name));
        fs::rename(&renamed_path, &original_path).context("restoring renamed profile")?;
        if active_profile == renamed_to {
            config::write_active_profile(&undo.profile_name)?;
        }
        return Ok(());
    }
    let profile_path = config::profiles_dir().join(format!("{}.toml", undo.profile_name));
    keys::save(&profile_path, &undo.profile_previous)?;

    if undo.live_previous.is_some() && undo.active_profile_after == active_profile {
        let config_path = config::config_path();
        if let Some(live_previous) = &undo.live_previous {
            keys::save(&config_path, live_previous)?;
            config::write_active_profile(&undo.active_profile)?;
            switch::reload_config()?;
        }
    }
    Ok(())
}

/// `conflict` carries the labels of the rows sharing this shortcut; it is shown
/// after the description so the partner is known without scrolling to it.
#[cfg(test)]
fn render_row(
    row: &Row,
    width: usize,
    highlight: Option<RowHighlight>,
    conflict: Option<&str>,
) -> String {
    render_row_with_key_width(row, width, KEY_COL, highlight, conflict)
}

fn render_row_with_key_width(
    row: &Row,
    width: usize,
    key_width: usize,
    highlight: Option<RowHighlight>,
    conflict: Option<&str>,
) -> String {
    match row {
        Row::Blank => String::new(),
        Row::Section(name) => format!(
            "{LEFT_PAD}{BOLD}{PURPLE}{}{RESET}",
            fit_line(name, width.saturating_sub(LEFT_PAD.len()))
        ),
        Row::Entry {
            key, description, ..
        } => {
            let key_width = key_width.min(width.saturating_sub(LEFT_PAD.len() + 4));
            let key = fit_line(key, key_width);
            let description = &match conflict {
                Some(note) => format!("{description} \u{26a0} {note}"),
                None => description.clone(),
            };
            let desc_width = (width.saturating_sub(LEFT_PAD.len() + key_width + 1)).max(4);
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
                if matches!(highlight, RowHighlight::Editing) {
                    format!(
                        "{LEFT_PAD}{BOLD}{FG_FOCUS_BLUE}{key:<key_width$}\x1b[22m\x1b[39m {desc}"
                    )
                } else if matches!(highlight, RowHighlight::Saved) {
                    format!("{LEFT_PAD}{BOLD}{FG_BLACK}{key:<key_width$}\x1b[22m {desc}")
                } else if matches!(highlight, RowHighlight::Listening) {
                    format!("{LEFT_PAD}{FG_WHITE}{key:<key_width$}\x1b[22m\x1b[39m {desc}")
                } else {
                    format!(
                        "{BG_RED}{FG_WHITE}{LEFT_PAD}{BOLD}{key:<key_width$}{RESET}{BG_RED}{FG_WHITE} {desc}{RESET}"
                    )
                }
            } else if conflict.is_some() {
                format!("{RED}{LEFT_PAD}{BOLD}{key:<key_width$}{RESET}{RED} {desc}{RESET}")
            } else {
                format!("{LEFT_PAD}{BOLD}{FG_WHITE}{key:<key_width$}{RESET} {desc}")
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
    staged_undo: Option<(Target, Option<Item>)>,
    /// Edit-mode quick filter: show only duplicated rows (plus the selection).
    duplicates_only: bool,
    /// Name being typed for a new profile (shift+N), if any.
    naming: Option<String>,
    /// New name being entered for the viewed profile.
    renaming: Option<String>,
    /// One-shot message for the hint line, cleared on the next key.
    notice: Option<String>,
    /// Profile awaiting deletion confirmation (shift+D).
    deleting: Option<String>,
    /// Brief in-view toast shown after switching the active profile.
    profile_toast_until: Option<Instant>,
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
            naming: None,
            renaming: None,
            notice: None,
            deleting: None,
            profile_toast_until: None,
        })
    }

    fn reload_viewed(&mut self) -> Result<()> {
        let offset = self.offset;
        let live = keys::load(&config::config_path())?;
        let displaced = config::load_displaced_plugin_keys()?;
        self.rows = match (&self.viewed_profile, &self.working_profile) {
            (Some(_), Some(profile)) => build_rows(profile, &live, &displaced),
            (Some(name), None) => {
                let profile_path = config::profiles_dir().join(format!("{name}.toml"));
                let profile = keys::load(&profile_path)?;
                build_rows(&profile, &live, &displaced)
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
            .filter(|index| duplicates.contains(index) && row_target(&self.rows[*index]).is_some())
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
            self.profile_toast_until = Some(Instant::now() + Duration::from_secs(2));
        }
        Ok(())
    }

    fn first_editable_row(&self) -> Option<usize> {
        self.filtered
            .iter()
            .copied()
            .find(|&index| row_target(&self.rows[index]).is_some())
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
        let footer_lines =
            edit_footer_lines(EditMode::Selecting { row }, cols as usize, false).len();
        let viewport = (term_rows as usize)
            .saturating_sub(10 + footer_lines)
            .max(1);
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
            .filter(|&index| row_target(&self.rows[index]).is_some())
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

    /// Create the profile named in `naming` as a copy of the viewed one, then
    /// view it. Errors are shown in the hint line and keep the name prompt.
    fn create_profile(&mut self) -> Result<()> {
        let Some(name) = self.naming.clone() else {
            return Ok(());
        };
        let error = if !save::valid_name(&name) {
            Some("use only letters, digits, - and _".to_string())
        } else if self.profiles.contains(&name) {
            Some(format!("profile '{name}' already exists"))
        } else {
            None
        };
        if let Some(error) = error {
            self.naming = None;
            self.notice = Some(error);
            return Ok(());
        }
        let base = self
            .viewed_profile
            .clone()
            .unwrap_or_else(|| DEFAULT_PROFILE.to_string());
        let dir = config::profiles_dir();
        let profile = keys::load(&dir.join(format!("{base}.toml")))?;
        keys::save(&dir.join(format!("{name}.toml")), &profile)?;
        self.naming = None;
        self.profiles = config::list_profiles()?;
        self.viewed_profile = Some(name.clone());
        self.notice = Some(format!("created '{name}' from '{base}'; press e to edit"));
        self.reload_viewed()
    }

    fn request_rename_profile(&mut self) {
        if self.has_pending_changes() {
            self.notice = Some("save or discard keybind edits before renaming this profile".into());
            return;
        }
        if self.editing.is_some() {
            self.discard_edit();
        }
        match self.viewed_profile.as_deref() {
            Some(DEFAULT_PROFILE) => {
                self.notice = Some("the default profile cannot be renamed".to_string());
            }
            Some(_) => self.renaming = Some(String::new()),
            None => {}
        }
    }

    fn rename_profile(&mut self) -> Result<()> {
        let Some(new_name) = self.renaming.clone() else {
            return Ok(());
        };
        let Some(old_name) = self.viewed_profile.clone() else {
            self.renaming = None;
            return Ok(());
        };
        if !save::valid_name(&new_name) {
            self.renaming = None;
            self.notice = Some("use only letters, digits, - and _".to_string());
            return Ok(());
        }
        if new_name == old_name {
            self.renaming = None;
            self.notice = Some(format!("'{old_name}' is already named that"));
            return Ok(());
        }
        if self.profiles.contains(&new_name) {
            self.renaming = None;
            self.notice = Some(format!("profile '{new_name}' already exists"));
            return Ok(());
        }

        let profiles_dir = config::profiles_dir();
        let old_path = profiles_dir.join(format!("{old_name}.toml"));
        let new_path = profiles_dir.join(format!("{new_name}.toml"));
        if new_path.exists() {
            self.renaming = None;
            self.notice = Some(format!("profile '{new_name}' already exists"));
            return Ok(());
        }
        fs::rename(&old_path, &new_path).context("renaming profile")?;
        let was_active = self.active_profile == old_name;
        if was_active {
            if let Err(error) = config::write_active_profile(&new_name) {
                fs::rename(&new_path, &old_path).context("rolling back profile rename")?;
                return Err(error);
            }
            self.active_profile = new_name.clone();
        }

        let undo = UndoEntry {
            profile_name: old_name.clone(),
            renamed_to: Some(new_name.clone()),
            profile_previous: keys::load(&new_path)?,
            live_previous: None,
            active_profile: if was_active {
                old_name.clone()
            } else {
                self.active_profile.clone()
            },
            active_profile_after: self.active_profile.clone(),
        };
        save_undo(&undo)?;
        self.last_undo = Some(undo);
        self.renaming = None;
        self.profiles = config::list_profiles()?;
        self.viewed_profile = Some(new_name.clone());
        self.notice = Some(format!(
            "renamed '{old_name}' to '{new_name}'; press u to undo"
        ));
        self.reload_viewed()
    }

    fn request_delete_profile(&mut self) {
        if self.has_pending_changes() {
            self.notice = Some("save or discard keybind edits before deleting this profile".into());
            return;
        }
        if self.editing.is_some() {
            self.discard_edit();
        }
        self.searching = false;
        self.query.clear();
        self.refilter();
        match self.viewed_profile.as_deref() {
            Some(DEFAULT_PROFILE) => {
                self.notice = Some("the default profile cannot be deleted".to_string());
            }
            Some(name) => self.deleting = Some(name.to_string()),
            None => {}
        }
    }

    /// Delete the profile confirmed in `deleting`. Deleting the active profile
    /// switches Herdr to the protected default profile first.
    fn delete_profile(&mut self) -> Result<()> {
        let Some(name) = self.deleting.clone() else {
            return Ok(());
        };
        self.deleting = None;
        if name == DEFAULT_PROFILE {
            self.notice = Some("the default profile cannot be deleted".to_string());
            return Ok(());
        }

        let profile_path = config::profiles_dir().join(format!("{name}.toml"));
        let profile_previous = keys::load(&profile_path)?;
        let was_active = self.active_profile == name;
        fs::remove_file(&profile_path).context("deleting profile")?;
        let live_previous = if was_active {
            let default =
                keys::load(&config::profiles_dir().join(format!("{DEFAULT_PROFILE}.toml")))?;
            Some(switch::apply_to_config(&default)?)
        } else {
            None
        };
        if was_active {
            self.active_profile = DEFAULT_PROFILE.to_string();
            config::write_active_profile(DEFAULT_PROFILE)?;
            switch::reload_config()?;
        }

        let active_profile_after = self.active_profile.clone();
        let undo = UndoEntry {
            profile_name: name.clone(),
            renamed_to: None,
            profile_previous,
            live_previous,
            active_profile: if was_active {
                name.clone()
            } else {
                self.active_profile.clone()
            },
            active_profile_after,
        };
        save_undo(&undo)?;
        self.last_undo = Some(undo);
        let previous_index = self
            .viewed_profile
            .as_deref()
            .and_then(|viewed| self.profiles.iter().position(|profile| profile == viewed))
            .unwrap_or(0);
        self.profiles = config::list_profiles()?;
        if was_active {
            self.viewed_profile = Some(DEFAULT_PROFILE.to_string());
        } else {
            self.viewed_profile = self.profiles.get(previous_index).cloned();
        }
        self.notice = Some(format!("deleted '{name}'; press u to undo"));
        self.reload_viewed()
    }

    /// Stage an empty binding (`key = ""`) for the selected row.
    fn unset_selected(&mut self) -> Result<()> {
        let Some(EditMode::Selecting { row }) = self.editing else {
            return Ok(());
        };
        // Herdr needs a prefix key; everything else may be unbound.
        if row_config_key(&self.rows[row]) == Some("prefix") {
            return Ok(());
        }
        self.save_edit(row, "")
    }

    fn begin_manual_edit(&mut self) -> Result<()> {
        let Some(EditMode::Selecting { row }) = self.editing else {
            return Ok(());
        };
        let Some(target) = row_target(&self.rows[row]).cloned() else {
            return Ok(());
        };
        let Some(profile_name) = self.viewed_profile.as_deref() else {
            return Ok(());
        };
        self.saved_row = None;
        self.manual_binding = match &target {
            Target::Key(config_key) => self.current_profile_binding(profile_name, config_key)?,
            // Plugin rows already display the effective binding.
            Target::Command(_) => match &self.rows[row] {
                Row::Entry { key, .. } if key != "unset" => Some(key.clone()),
                _ => None,
            },
        }
        .unwrap_or_default();
        self.pending_binding = None;
        self.editing = Some(EditMode::Manual { row });
        Ok(())
    }

    fn save_edit(&mut self, row: usize, binding: &str) -> Result<()> {
        let Some(target) = row_target(&self.rows[row]).cloned() else {
            return Ok(());
        };
        let Some(profile) = self.working_profile.as_mut() else {
            return Ok(());
        };
        let previous = target.get_item(profile);
        target.set(profile, binding)?;
        self.staged_undo = Some((target, previous));
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
        let value = keys::binding_overrides(&profile)
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
        if let (Some(profile), Some((target, previous))) =
            (self.working_profile.as_mut(), self.staged_undo.take())
        {
            match previous {
                Some(item) => target.set_item(profile, item),
                None => target.remove(profile),
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
        self.active_profile = config::read_active_profile().unwrap_or_else(|| "?".to_string());
        self.profiles = config::list_profiles()?;
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
        if row_target(&self.rows[row]).is_none() {
            self.editing = Some(EditMode::Selecting { row });
            return Ok(());
        }
        let is_prefix = row_config_key(&self.rows[row]) == Some("prefix");
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
            if !prefix
                .split(", ")
                .any(|value| binding.eq_ignore_ascii_case(value))
            {
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
        if prefix
            .split(", ")
            .any(|value| binding.eq_ignore_ascii_case(value))
        {
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

/// Truncate `text` to `width` columns with an ellipsis when it must stay on one
/// line, such as while typing a profile name or key binding.
fn fit_line(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('\u{2026}');
    cut
}

fn wrap_words(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let word = if word.chars().count() > width {
            fit_line(word, width)
        } else {
            word.to_string()
        };
        let extra = usize::from(!line.is_empty());
        if !line.is_empty() && line.chars().count() + extra + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// Rows sharing the shortcut of the row selected in edit mode, if any.
fn selected_conflict(state: &ViewState) -> Option<Vec<usize>> {
    let Some(EditMode::Selecting { row }) = state.editing else {
        return None;
    };
    duplicate_partners(&state.rows).remove(&row)
}

fn ansi_width(text: &str) -> usize {
    let mut width = 0;
    let mut escape = false;
    for ch in text.chars() {
        if escape {
            if ch == 'm' {
                escape = false;
            }
        } else if ch == '\u{1b}' {
            escape = true;
        } else {
            width += 1;
        }
    }
    width
}

fn pad_ansi(text: &str, width: usize) -> String {
    let visible = ansi_width(text);
    format!("{text}{}", " ".repeat(width.saturating_sub(visible)))
}

fn fit_ansi_line(text: &str, width: usize) -> String {
    if ansi_width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut visible = 0;
    let mut escape = false;
    for ch in text.chars() {
        if ch == '\u{1b}' {
            escape = true;
        } else if escape {
            if ch == 'm' {
                escape = false;
            }
        } else {
            if visible == width - 1 {
                break;
            }
            visible += 1;
        }
        result.push(ch);
    }
    result.push('\u{2026}');
    result.push_str(RESET);
    result
}

fn panel_rule(cols: usize, left: char, right: char) -> String {
    let fill = "─".repeat(cols.saturating_sub(2));
    format!("{BORDER}{left}{fill}{right}{RESET}")
}

fn panel_title_rule(cols: usize, title: &str) -> String {
    let title = format!(" {title} ");
    let leading = 2.min(cols.saturating_sub(2 + title.len()));
    let trailing = cols.saturating_sub(2 + leading + title.len());
    format!(
        "{BORDER}╭{}{RESET}{BOLD}{PURPLE}{title}{RESET}{BORDER}{}╮{RESET}",
        "─".repeat(leading),
        "─".repeat(trailing),
    )
}

fn panel_line(text: &str, cols: usize, background: Option<&str>) -> String {
    let inner_width = cols.saturating_sub(4);
    let padded = pad_ansi(text, inner_width);
    match background {
        Some(background) => {
            format!("{BORDER}│{RESET}{background} {padded} {RESET}{BORDER}│{RESET}")
        }
        None => format!("{BORDER}│{RESET} {padded} {BORDER}│{RESET}"),
    }
}

fn text_field(
    cols: usize,
    title: &str,
    value: &str,
    placeholder: Option<&str>,
    focused: bool,
) -> String {
    let top = panel_title_rule(cols, title);

    let max_query = cols.saturating_sub(8 + usize::from(focused));
    let visible_value: String = value
        .chars()
        .rev()
        .take(max_query)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let content = if value.is_empty() && !focused {
        format!(
            "{DIM}{}{RESET}",
            fit_ansi_line(placeholder.unwrap_or(""), cols.saturating_sub(8))
        )
    } else if focused {
        format!("{visible_value}{YELLOW}\u{2588}{RESET}")
    } else {
        visible_value
    };
    let input = panel_line(&format!("{CYAN}>{RESET} {content}"), cols, None);
    format!("{top}\r\n{input}\r\n{}\r\n", panel_rule(cols, '╰', '╯'))
}

fn search_field(cols: usize, query: &str, focused: bool) -> String {
    let placeholder =
        format!("press {RESET}{FG_WHITE}/{RESET}{DIM} to filter by command or shortcut");
    text_field(cols, "filter", query, Some(&placeholder), focused)
}

fn instruction_field(cols: usize, title: &str, hint: &str) -> String {
    let hint = fit_ansi_line(hint, cols.saturating_sub(8));
    let content = panel_line(&format!("{CYAN}>{RESET} {DIM}{hint}{RESET}"), cols, None);
    format!(
        "{}\r\n{content}\r\n{}\r\n",
        panel_title_rule(cols, title),
        panel_rule(cols, '╰', '╯')
    )
}

fn render(out: &mut impl Write, state: &mut ViewState) -> Result<()> {
    let (cols, term_rows) = terminal::size()?;
    let cols = cols as usize;

    let confirming = matches!(state.editing, Some(EditMode::Confirming { .. }));
    let prefix_listening = match state.editing {
        Some(EditMode::Listening { row, .. }) => row_config_key(&state.rows[row]) == Some("prefix"),
        _ => false,
    };
    let browsing = state.editing.is_none()
        && !state.searching
        && state.naming.is_none()
        && state.renaming.is_none()
        && state.deleting.is_none();
    let show_search_field = state.searching || (browsing && state.notice.is_none());
    let footer_lines = if browsing {
        browse_footer_lines(cols)
    } else if state.searching {
        filter_footer_lines(cols)
    } else if let Some(mode) = state.editing {
        edit_footer_lines(mode, cols, prefix_listening)
    } else if state.renaming.is_some() {
        rename_footer_lines(cols)
    } else if state.deleting.is_some() {
        delete_footer_lines(cols)
    } else {
        naming_footer_lines(cols)
    };
    let inner_width = cols.saturating_sub(4);

    let banner = format!(" active profile: {} ", state.active_profile);
    let pad = cols.saturating_sub(banner.chars().count() + 2) / 2;

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
        "{CYAN}{BG_CYAN}{FG_BLACK}{BOLD}{banner}{RESET}{CYAN}{RESET}\x1b[K\r\n"
    ));
    buf.push_str("\x1b[K\r\n");
    buf.push_str(LEFT_PAD);
    buf.push_str(&tabs);
    buf.push_str("\x1b[K\r\n");
    let readonly_notice = browsing
        && state.notice.is_none()
        && state.viewed_profile.as_deref() == Some(DEFAULT_PROFILE);
    if readonly_notice {
        buf.push_str(&format!(
            "{LEFT_PAD}{DIM}default profile is read-only{RESET}\x1b[K\r\n"
        ));
    } else {
        buf.push_str("\r\n");
    }
    let mut extra_status_lines = 0;
    if show_search_field {
        buf.push_str(&search_field(cols, &state.query, state.searching));
        extra_status_lines = 1;
    } else if matches!(state.editing, Some(EditMode::Manual { .. })) {
        buf.push_str(&text_field(
            cols,
            "input",
            &state.manual_binding,
            None,
            true,
        ));
        extra_status_lines = 1;
    } else if let Some(EditMode::Listening { prefix_seen, .. }) = state.editing {
        let value =
            state
                .pending_binding
                .as_deref()
                .unwrap_or(if prefix_seen { "prefix+" } else { "" });
        buf.push_str(&text_field(
            cols,
            "capture",
            value,
            None,
            state.pending_binding.is_none(),
        ));
        extra_status_lines = 1;
    } else if let Some(name) = &state.naming {
        let base = state.viewed_profile.as_deref().unwrap_or(DEFAULT_PROFILE);
        buf.push_str(&format!(
            "{LEFT_PAD}{BOLD}new profile from {base}:{RESET} {name}\u{2588}\x1b[K\r\n\r\n",
        ));
    } else if let Some(name) = &state.renaming {
        buf.push_str(&text_field(cols, "rename profile", name, None, true));
        extra_status_lines = 1;
    } else if let Some(name) = state.deleting.as_deref() {
        let key = |binding: &str| format!("{RESET}{FG_WHITE}{binding}{RESET}{DIM}");
        let hint = format!(
            "delete '{name}'? {} confirm · {} cancel",
            key("y/enter"),
            key("n/esc")
        );
        buf.push_str(&instruction_field(cols, "delete profile", &hint));
        extra_status_lines = 1;
    } else if let Some(editing) = state.editing {
        let key = |binding: &str| format!("{RESET}{FG_WHITE}{binding}{RESET}{DIM}");
        let hint = if confirming && state.has_unresolved_duplicates() {
            format!(
                "resolve red duplicates to save; {} discard; {} back",
                key("n"),
                key("esc")
            )
        } else if confirming {
            format!(
                "save changes? {} yes; {} discard; {} back",
                key("y/enter"),
                key("n"),
                key("esc")
            )
        } else if let Some(others) = selected_conflict(state) {
            format!(
                "also used by {}; {} next duplicate; {} duplicates only",
                conflict_note(&state.rows, &others),
                key("d"),
                key("f")
            )
        } else if state.has_unresolved_duplicates() {
            format!(
                "duplicates are red; resolve them to save, or {} to discard",
                key("esc")
            )
        } else {
            match editing {
                EditMode::Selecting { .. } => format!(
                    "{} listen · {} manual text · {} leave edit mode",
                    key("enter"),
                    key("m"),
                    key("esc")
                ),
                EditMode::Manual { .. }
                | EditMode::Listening { .. }
                | EditMode::Confirming { .. } => unreachable!(),
            }
        };
        let title = if confirming { "save" } else { "edit" };
        buf.push_str(&instruction_field(cols, title, &hint));
        extra_status_lines = 1;
    } else {
        let hint = if let Some(notice) = &state.notice {
            notice.as_str()
        } else {
            "press / to filter by command or shortcut"
        };
        let lines = wrap_words(hint, cols.saturating_sub(LEFT_PAD.len()));
        extra_status_lines = lines.len().saturating_sub(1);
        for line in lines {
            buf.push_str(&format!("{LEFT_PAD}{DIM}{line}{RESET}\x1b[K\r\n"));
        }
        buf.push_str("\r\n");
    }
    let viewport = (term_rows as usize)
        .saturating_sub(9 + extra_status_lines + footer_lines.len())
        .max(1);
    let max_offset = state.filtered.len().saturating_sub(viewport);
    state.offset = state.offset.min(max_offset);
    buf.push_str(&panel_title_rule(cols, "bindings"));
    buf.push_str("\r\n");
    let key_width = (inner_width / 3).max(8);
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
    let mut visible_rows = state.filtered.iter().copied().skip(state.offset);
    for position in 0..viewport {
        let row_index = visible_rows.next();
        let row_highlight = row_index.and_then(|idx| {
            selected_row
                .filter(|(row, _)| *row == idx)
                .map(|(_, highlight)| highlight)
        });
        let left = if let Some(idx) = row_index {
            render_row_with_key_width(
                &state.rows[idx],
                inner_width,
                key_width,
                row_highlight,
                conflicts
                    .get(&idx)
                    .map(|others| conflict_note(&state.rows, others))
                    .as_deref(),
            )
        } else if position == 0 && state.filtered.is_empty() {
            format!("{LEFT_PAD}{DIM}no matches{RESET}")
        } else {
            String::new()
        };
        let background = match row_highlight {
            Some(RowHighlight::Editing | RowHighlight::Listening) => Some(BG_GREY),
            Some(RowHighlight::Saved) => Some(BG_SAVED),
            Some(RowHighlight::Conflict) | None => None,
        };
        buf.push_str(&panel_line(&left, cols, background));
        buf.push_str("\r\n");
    }
    buf.push_str(&panel_rule(cols, '╰', '╯'));
    buf.push_str("\r\n\r\n");
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
    if state
        .profile_toast_until
        .is_some_and(|deadline| Instant::now() < deadline)
    {
        render_profile_toast(
            out,
            &state.profiles,
            &state.active_profile,
            cols,
            term_rows as usize,
        )?;
    }
    Ok(())
}

fn render_profile_toast(
    out: &mut impl Write,
    profiles: &[String],
    active_profile: &str,
    cols: usize,
    rows: usize,
) -> Result<()> {
    if cols < 20 || rows < 9 {
        return Ok(());
    }

    let width = 34.min(cols - 6);
    let inner_width = width - 2;
    let visible_count = profiles.len().min(rows - 8);
    let height = visible_count + 4;
    let left = (cols - width) / 2;
    let top = (rows - height) / 2;

    // Clear one row and two columns around the box so the underlying keybinds
    // do not visually run into its border.
    let blank = " ".repeat(width + 4);
    for row in top - 1..=top + height {
        write!(out, "\x1b[{};{}H{RESET}{blank}", row + 1, left - 1)?;
    }

    let title = format!(" {} ", fit_line("switch keybind profile", inner_width - 2));
    let title_width = title.chars().count();
    let remaining = inner_width.saturating_sub(title_width);
    let top_line = format!(
        "{BORDER}╭{}{RESET}{BOLD}{BLUE}{}{RESET}{BORDER}{}╮{RESET}",
        "─".repeat(remaining / 2),
        title,
        "─".repeat(remaining - remaining / 2),
    );
    write!(out, "\x1b[{};{}H{top_line}", top + 1, left + 1)?;

    let blank_line = format!(
        "{BORDER}│{RESET}{}{BORDER}│{RESET}",
        " ".repeat(inner_width)
    );
    write!(out, "\x1b[{};{}H{blank_line}", top + 2, left + 1)?;

    for (index, name) in profiles.iter().take(visible_count).enumerate() {
        let selected = name == active_profile;
        let name = fit_line(name, inner_width.saturating_sub(8));
        let label = if selected {
            format!("  {PILL_GREEN}{BG_GREEN}{FG_BLACK}{BOLD} {name} {RESET}{PILL_GREEN}{RESET}")
        } else {
            format!("    {name}")
        };
        let line = format!(
            "{BORDER}│{RESET}{}{BORDER}│{RESET}",
            pad_ansi(&label, inner_width)
        );
        write!(out, "\x1b[{};{}H{line}", top + index + 3, left + 1)?;
    }

    write!(
        out,
        "\x1b[{};{}H{blank_line}",
        top + visible_count + 3,
        left + 1
    )?;
    let bottom_line = format!("{BORDER}╰{}╯{RESET}", "─".repeat(inner_width),);
    write!(out, "\x1b[{};{}H{bottom_line}", top + height, left + 1)?;
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

        // Wait until input or the profile-switch toast expires, then drain
        // already-queued events before redrawing once.
        let timeout = state
            .profile_toast_until
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_secs(86_400));
        if !event::poll(timeout)? {
            state.profile_toast_until = None;
            continue;
        }
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
                            cols,
                            m.column,
                        ) {
                            state.select_profile(index)?;
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left)
                        if m.row == SEARCH_INPUT_ROW
                            && state.editing.is_none()
                            && state.naming.is_none()
                            && state.renaming.is_none()
                            && state.deleting.is_none()
                            && state.notice.is_none() =>
                    {
                        state.searching = true;
                    }
                    _ => {}
                },
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    state.notice = None;
                    let can_request_profile_action = state.naming.is_none()
                        && state.renaming.is_none()
                        && state.deleting.is_none()
                        && !state.searching
                        && !matches!(
                            state.editing,
                            Some(EditMode::Listening { .. })
                                | Some(EditMode::Manual { .. })
                                | Some(EditMode::Confirming { .. })
                        );
                    if can_request_profile_action && shifted_char(&k, 'D') {
                        state.request_delete_profile();
                    } else if can_request_profile_action && k.code == KeyCode::Char('r') {
                        state.request_rename_profile();
                    } else if k.code == KeyCode::Char('c')
                        && k.modifiers.contains(KeyModifiers::CONTROL)
                    {
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
                                KeyCode::Char('x') => state.unset_selected()?,
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
                    } else if let Some(name) = state.naming.as_mut() {
                        match k.code {
                            KeyCode::Esc => state.naming = None,
                            KeyCode::Enter => state.create_profile()?,
                            KeyCode::Backspace => {
                                name.pop();
                            }
                            KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                                name.clear();
                            }
                            KeyCode::Char(c)
                                if c.is_ascii_alphanumeric() || c == '-' || c == '_' =>
                            {
                                name.push(c);
                            }
                            _ => {}
                        }
                    } else if let Some(name) = state.renaming.as_mut() {
                        match k.code {
                            KeyCode::Esc => state.renaming = None,
                            KeyCode::Enter => state.rename_profile()?,
                            KeyCode::Backspace => {
                                name.pop();
                            }
                            KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                                name.clear();
                            }
                            KeyCode::Char(c)
                                if c.is_ascii_alphanumeric() || c == '-' || c == '_' =>
                            {
                                name.push(c);
                            }
                            _ => {}
                        }
                    } else if state.deleting.is_some() {
                        match k.code {
                            KeyCode::Char('y') | KeyCode::Enter => state.delete_profile()?,
                            KeyCode::Char('n') | KeyCode::Esc => state.deleting = None,
                            _ => {}
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
                            _ if shifted_char(&k, 'K') => state.switch_profile()?,
                            _ if shifted_char(&k, 'N') => state.naming = Some(String::new()),
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
        let lines = browse_footer_lines(76);
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
    fn shifted_shortcut_accepts_both_terminal_key_representations() {
        let uppercase = KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT);
        let lowercase = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::SHIFT);
        let unshifted = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);

        assert!(shifted_char(&uppercase, 'D'));
        assert!(shifted_char(&lowercase, 'D'));
        assert!(!shifted_char(&unshifted, 'D'));
    }

    #[test]
    fn bindings_frame_fits_the_popup_width() {
        let cols: usize = 76;
        assert_eq!(ansi_width(&panel_rule(cols, '╭', '╮')), cols);
        assert_eq!(ansi_width(&panel_rule(cols, '╰', '╯')), cols);
        assert_eq!(ansi_width(&panel_line("BINDINGS", cols, None)), cols);
    }

    #[test]
    fn edit_row_highlight_uses_the_mode_color() {
        let row = Row::Entry {
            target: Some(Target::Key("new_tab".to_string())),
            key: "prefix+c".to_string(),
            description: "open a tab".to_string(),
        };

        let listening = panel_line(
            &render_row(&row, 76, Some(RowHighlight::Listening), None),
            76,
            Some(BG_GREY),
        );
        assert!(listening.contains(BG_GREY));
        assert!(listening.contains(FG_WHITE));

        let saved = panel_line(
            &render_row(&row, 76, Some(RowHighlight::Saved), None),
            76,
            Some(BG_SAVED),
        );
        assert!(saved.contains(BG_SAVED));
        assert!(saved.contains(FG_BLACK));
    }

    #[test]
    fn duplicate_rows_include_both_bindings_and_ignore_unset_rows() {
        let rows = vec![
            Row::Entry {
                target: Some(Target::Key("one".to_string())),
                key: "prefix+x".to_string(),
                description: "one".to_string(),
            },
            Row::Entry {
                target: Some(Target::Key("two".to_string())),
                key: "PREFIX+X".to_string(),
                description: "two".to_string(),
            },
            Row::Entry {
                target: None,
                key: "unset".to_string(),
                description: "three".to_string(),
            },
        ];
        let conflicts = duplicate_rows(&rows);
        assert_eq!(conflicts, HashSet::from([0, 1]));
    }

    #[test]
    fn each_prefix_in_an_array_participates_in_conflict_detection() {
        let profile: DocumentMut =
            "[keys]\nprefix = [\"ctrl+space\", \"ctrl+s\"]\nhelp = \"ctrl+s\"\n"
                .parse()
                .unwrap();
        let rows = build_rows(&profile, &profile, &DocumentMut::new());
        let prefix = rows
            .iter()
            .position(|row| row_config_key(row) == Some("prefix"))
            .unwrap();
        let help = rows
            .iter()
            .position(|row| row_config_key(row) == Some("help"))
            .unwrap();
        assert_eq!(duplicate_partners(&rows).get(&prefix), Some(&vec![help]));
    }

    #[test]
    fn navigation_shortcuts_do_not_conflict_with_terminal_mode() {
        let profile: DocumentMut = "[keys]\nnavigate_pane_left = \"h\"\nnew_tab = \"h\"\n"
            .parse()
            .unwrap();
        let rows = build_rows(&profile, &profile, &DocumentMut::new());
        let navigation = rows
            .iter()
            .position(|row| row_config_key(row) == Some("navigate_pane_left"))
            .unwrap();
        let new_tab = rows
            .iter()
            .position(|row| row_config_key(row) == Some("new_tab"))
            .unwrap();
        assert!(!duplicate_partners(&rows).contains_key(&navigation));
        assert!(!duplicate_partners(&rows).contains_key(&new_tab));
    }

    #[test]
    fn duplicate_row_text_and_selector_use_ansi_red() {
        let row = Row::Entry {
            target: Some(Target::Key("one".to_string())),
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
            naming: None,
            renaming: None,
            notice: None,
            deleting: None,
            profile_toast_until: None,
        }
    }

    #[test]
    fn unresolved_duplicate_blocks_saving_but_not_leaving() {
        let rows = vec![
            Row::Entry {
                target: Some(Target::Key("one".to_string())),
                key: "prefix+x".to_string(),
                description: "one".to_string(),
            },
            Row::Entry {
                target: Some(Target::Key("two".to_string())),
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
            target: Some(Target::Key("one".to_string())),
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
                target: Some(Target::Key("focus_pane_up".to_string())),
                key: "prefix+k".to_string(),
                description: "focus pane up".to_string(),
            },
            Row::Entry {
                target: Some(Target::Key("other".to_string())),
                key: "prefix+o".to_string(),
                description: "other".to_string(),
            },
            Row::Section("custom".to_string()),
            Row::Entry {
                target: None,
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
