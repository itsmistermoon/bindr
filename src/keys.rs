//! Surgical edits to the `[keys]` table in Herdr's `config.toml`, leaving
//! everything else -- theme, ui, and every `[[keys.command]]` block from any
//! plugin -- byte-for-byte untouched where possible.
//!
//! `[keys]`, `[keys.indexed]`, and `[[keys.command]]` are all, syntactically,
//! the same top-level "keys" table in TOML's tree; toml_edit lets us touch
//! only the plain scalar fields (and the `indexed` subtable) while leaving
//! the `command` array-of-tables node completely alone.

use crate::keybinds_data;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use toml_edit::{DocumentMut, Item, Table, value};

pub fn load(path: &Path) -> Result<DocumentMut> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {path:?}"))?;
    text.parse::<DocumentMut>()
        .with_context(|| format!("parsing {path:?}"))
}

pub fn save(path: &Path, doc: &DocumentMut) -> Result<()> {
    fs::write(path, doc.to_string()).with_context(|| format!("writing {path:?}"))
}

fn keys_table_mut(doc: &mut DocumentMut) -> &mut Table {
    if doc.get("keys").is_none() {
        doc["keys"] = Item::Table(Table::new());
    }
    doc["keys"].as_table_mut().expect("keys must be a table")
}

/// Set one scalar binding in `[keys]`, preserving the rest of the document.
pub fn set_scalar(doc: &mut DocumentMut, key: &str, binding: &str) {
    keys_table_mut(doc).insert(key, value(binding));
}

/// Replace every non-"command" entry in the live `[keys]` table with the
/// entries from `profile`'s `[keys]` table (or clear them all if the
/// profile has none), then apply the profile's `[plugin_keys]` to the
/// matching `[[keys.command]]` blocks (see `apply_plugin_keys`).
pub fn apply_profile(doc: &mut DocumentMut, profile: &DocumentMut, displaced: &mut DocumentMut) {
    let profile_entries: Vec<(String, Item)> = profile
        .get("keys")
        .and_then(|item| item.as_table())
        .map(|t| {
            t.iter()
                .filter(|(k, _)| *k != "command")
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect()
        })
        .unwrap_or_default();

    let live = keys_table_mut(doc);
    let stale: Vec<String> = live
        .iter()
        .filter(|(k, _)| *k != "command")
        .map(|(k, _)| k.to_string())
        .collect();
    for k in stale {
        live.remove(&k);
    }
    for (k, v) in profile_entries {
        live.insert(&k, v);
    }
    apply_plugin_keys(doc, profile, displaced);
}

/// Rebind `[[keys.command]]` blocks from the profile's `[plugin_keys]`
/// table (command id -> binding). Only each block's `key` field changes;
/// blocks are never created or removed, since plugins own them.
///
/// `displaced` maps command id -> the plugin's own binding while a profile
/// override is live, so switching to a profile without an override for that
/// command restores the plugin's binding instead of leaking the previous
/// profile's one.
pub fn apply_plugin_keys(
    doc: &mut DocumentMut,
    profile: &DocumentMut,
    displaced: &mut DocumentMut,
) {
    let Some(blocks) = doc
        .get_mut("keys")
        .and_then(|keys| keys.get_mut("command"))
        .and_then(Item::as_array_of_tables_mut)
    else {
        return;
    };
    for block in blocks.iter_mut() {
        let Some(id) = block
            .get("command")
            .and_then(Item::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let current = block
            .get("key")
            .and_then(Item::as_str)
            .unwrap_or("")
            .to_string();
        match plugin_binding(profile, &id) {
            Some(binding) => {
                if displaced.get(&id).is_none() {
                    displaced[id.as_str()] = value(current);
                }
                block["key"] = value(binding);
            }
            None => {
                if let Some(original) = displaced.remove(&id) {
                    block["key"] = original;
                }
            }
        }
    }
}

/// A fresh document containing only the current `[keys]` table (minus
/// `command`), suitable for saving as a profile file.
pub fn extract_keys_only(doc: &DocumentMut) -> DocumentMut {
    let mut out = DocumentMut::new();
    let mut table = Table::new();
    if let Some(live) = doc.get("keys").and_then(|i| i.as_table()) {
        for (k, v) in live.iter() {
            if k != "command" {
                table.insert(k, v.clone());
            }
        }
    }
    out["keys"] = Item::Table(table);
    out
}

/// Build the neutral, read-only profile from the keybinds viewer's defaults.
pub fn default_profile() -> DocumentMut {
    let mut profile = DocumentMut::new();
    for section in keybinds_data::SECTIONS {
        for row in section.rows {
            if let Some(config_key) = row.config_key
                && !row.default.is_empty()
            {
                set_scalar(&mut profile, config_key, row.default);
            }
        }
    }
    profile
}

/// Scalar `[keys]` overrides as (config_key, value), skipping "command" and
/// any subtables (e.g. `[keys.indexed]`).
pub fn scalar_overrides(doc: &DocumentMut) -> Vec<(String, String)> {
    doc.get("keys")
        .and_then(|i| i.as_table())
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| {
                    if k == "command" {
                        return None;
                    }
                    v.as_str().map(|s| (k.to_string(), s.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One `[[keys.command]]` block as shown by the keybinds viewer.
pub struct CustomCommand {
    /// The block's `command` field, used as its identity in `[plugin_keys]`.
    pub id: Option<String>,
    pub key: String,
    pub description: String,
}

/// Every `[[keys.command]]` entry, in file order.
pub fn custom_commands(doc: &DocumentMut) -> Vec<CustomCommand> {
    doc.get("keys")
        .and_then(|i| i.get("command"))
        .and_then(|i| i.as_array_of_tables())
        .map(|aot| {
            aot.iter()
                .filter_map(|t| {
                    let key = t.get("key")?.as_str()?.to_string();
                    let description = t
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("custom command")
                        .to_string();
                    let id = t.get("command").and_then(Item::as_str).map(str::to_string);
                    Some(CustomCommand {
                        id,
                        key,
                        description,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// An editable binding: a built-in `[keys]` scalar, or a plugin command
/// rebound through the profile's `[plugin_keys]` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Key(String),
    Command(String),
}

impl Target {
    fn table_name(&self) -> &'static str {
        match self {
            Target::Key(_) => "keys",
            Target::Command(_) => "plugin_keys",
        }
    }

    fn table<'a>(&self, doc: &'a DocumentMut) -> Option<&'a Table> {
        doc.get(self.table_name()).and_then(Item::as_table)
    }

    fn table_mut<'a>(&self, doc: &'a mut DocumentMut) -> &'a mut Table {
        match self {
            Target::Key(_) => keys_table_mut(doc),
            Target::Command(_) => {
                if doc.get("plugin_keys").is_none() {
                    doc["plugin_keys"] = Item::Table(Table::new());
                }
                doc["plugin_keys"]
                    .as_table_mut()
                    .expect("plugin_keys must be a table")
            }
        }
    }

    fn name(&self) -> &str {
        match self {
            Target::Key(name) | Target::Command(name) => name,
        }
    }

    /// Cloned profile item, preserving its TOML type for undo.
    pub fn get_item(&self, doc: &DocumentMut) -> Option<Item> {
        self.table(doc).and_then(|t| t.get(self.name())).cloned()
    }

    pub fn get_str(&self, doc: &DocumentMut) -> Option<String> {
        self.table(doc)
            .and_then(|t| t.get(self.name()))
            .and_then(Item::as_str)
            .map(str::to_string)
    }

    pub fn set_item(&self, doc: &mut DocumentMut, item: Item) {
        let name = self.name().to_string();
        self.table_mut(doc).insert(&name, item);
    }

    pub fn set(&self, doc: &mut DocumentMut, binding: &str) {
        self.set_item(doc, value(binding));
    }

    pub fn remove(&self, doc: &mut DocumentMut) {
        let name = self.name().to_string();
        if let Some(table) = doc.get_mut(self.table_name()).and_then(Item::as_table_mut) {
            table.remove(&name);
        }
    }
}

/// The profile's binding for a plugin command, if it overrides one.
pub fn plugin_binding(profile: &DocumentMut, id: &str) -> Option<String> {
    Target::Command(id.to_string()).get_str(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(source: &str) -> DocumentMut {
        source.parse().expect("valid TOML")
    }

    #[test]
    fn profile_application_preserves_custom_commands_and_other_sections() {
        let mut live = document(
            r#"
[keys]
prefix = "ctrl+b"
new_tab = "prefix+c"

[[keys.command]]
key = "prefix+alt+g"
type = "popup"
command = "lazygit"

[theme]
name = "catppuccin"
"#,
        );
        let profile = document(
            r#"
[keys]
prefix = "ctrl+a"
close_tab = "prefix+shift+x"
"#,
        );

        apply_profile(&mut live, &profile, &mut DocumentMut::new());

        assert_eq!(live["keys"]["prefix"].as_str(), Some("ctrl+a"));
        assert_eq!(live["keys"]["close_tab"].as_str(), Some("prefix+shift+x"));
        assert!(live["keys"].as_table().unwrap().get("new_tab").is_none());
        let command = live["keys"]["command"]
            .as_array_of_tables()
            .expect("custom command array");
        let command = command.iter().next().expect("first custom command");
        assert_eq!(command["command"].as_str(), Some("lazygit"));
        assert_eq!(live["theme"]["name"].as_str(), Some("catppuccin"));
    }

    #[test]
    fn scalar_updates_are_reversible_without_touching_other_keys() {
        let mut doc = document(
            r#"
[keys]
prefix = "ctrl+b"
new_tab = "prefix+c"
"#,
        );
        let prefix = Target::Key("prefix".to_string());
        let previous = prefix.get_item(&doc);

        prefix.set(&mut doc, "ctrl+a");
        assert_eq!(doc["keys"]["prefix"].as_str(), Some("ctrl+a"));
        assert_eq!(doc["keys"]["new_tab"].as_str(), Some("prefix+c"));

        prefix.set_item(&mut doc, previous.expect("prefix exists"));
        assert_eq!(doc["keys"]["prefix"].as_str(), Some("ctrl+b"));
    }

    #[test]
    fn default_profile_contains_only_neutral_builtin_bindings() {
        let profile = default_profile();
        assert_eq!(profile["keys"]["prefix"].as_str(), Some("ctrl+b"));
        assert_eq!(profile["keys"]["new_tab"].as_str(), Some("prefix+c"));
        assert!(profile["keys"].get("open_worktree").is_none());
        assert!(profile["keys"].get("command").is_none());
    }

    const LIVE_WITH_PLUGINS: &str = r#"
[keys]
prefix = "ctrl+b"

[[keys.command]]
key = "prefix+k"
type = "plugin_action"
command = "herdr-bar.open"
description = "command bar"

[[keys.command]]
key = "prefix+t"
type = "plugin_action"
command = "termscope.open"
# END termscope
"#;

    fn command_key(doc: &DocumentMut, index: usize) -> String {
        doc["keys"]["command"]
            .as_array_of_tables()
            .unwrap()
            .get(index)
            .unwrap()["key"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn plugin_keys_rebind_only_the_key_and_restore_the_plugin_binding() {
        let mut live = document(LIVE_WITH_PLUGINS);
        let mut displaced = DocumentMut::new();
        let work = document(
            r#"
[keys]
prefix = "ctrl+a"

[plugin_keys]
"herdr-bar.open" = "cmd+k"
"#,
        );

        apply_profile(&mut live, &work, &mut displaced);
        assert_eq!(command_key(&live, 0), "cmd+k");
        assert_eq!(command_key(&live, 1), "prefix+t");
        assert_eq!(displaced["herdr-bar.open"].as_str(), Some("prefix+k"));
        assert!(live.to_string().contains("# END termscope"));

        // Switching again to an overriding profile keeps the original.
        apply_profile(&mut live, &work, &mut displaced);
        assert_eq!(displaced["herdr-bar.open"].as_str(), Some("prefix+k"));

        apply_profile(&mut live, &document("[keys]\n"), &mut displaced);
        assert_eq!(command_key(&live, 0), "prefix+k");
        assert!(displaced.get("herdr-bar.open").is_none());
    }

    #[test]
    fn plugin_targets_edit_the_plugin_keys_table() {
        let mut profile = document("[keys]\nprefix = \"ctrl+b\"\n");
        let target = Target::Command("herdr-bar.open".to_string());
        assert!(target.get_item(&profile).is_none());
        target.set(&mut profile, "");
        assert_eq!(
            plugin_binding(&profile, "herdr-bar.open").as_deref(),
            Some("")
        );
        target.remove(&mut profile);
        assert!(plugin_binding(&profile, "herdr-bar.open").is_none());
        assert_eq!(profile["keys"]["prefix"].as_str(), Some("ctrl+b"));
    }
}
