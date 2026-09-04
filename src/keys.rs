//! Surgical edits to the `[keys]` table in Herdr's `config.toml`, leaving
//! everything else -- theme, ui, and every `[[keys.command]]` block from any
//! plugin -- byte-for-byte untouched where possible.
//!
//! `[keys]`, `[keys.indexed]`, and `[[keys.command]]` are all, syntactically,
//! the same top-level "keys" table in TOML's tree; toml_edit lets us touch
//! only the plain scalar fields (and the `indexed` subtable) while leaving
//! the `command` array-of-tables node completely alone.

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

/// Return a cloned `[keys]` item, preserving its original TOML type for undo.
pub fn get_item(doc: &DocumentMut, key: &str) -> Option<Item> {
    doc.get("keys")
        .and_then(Item::as_table)
        .and_then(|table| table.get(key))
        .cloned()
}

/// Set one item in `[keys]`, preserving the rest of the document.
pub fn set_item(doc: &mut DocumentMut, key: &str, item: Item) {
    keys_table_mut(doc).insert(key, item);
}

/// Set one scalar binding in `[keys]`, preserving the rest of the document.
pub fn set_scalar(doc: &mut DocumentMut, key: &str, binding: &str) {
    set_item(doc, key, value(binding));
}

/// Remove one scalar binding from `[keys]`, preserving the rest of the table.
pub fn remove_scalar(doc: &mut DocumentMut, key: &str) {
    if let Some(table) = doc.get_mut("keys").and_then(Item::as_table_mut) {
        table.remove(key);
    }
}

/// Replace every non-"command" entry in the live `[keys]` table with the
/// entries from `profile`'s `[keys]` table (or clear them all if the
/// profile has none). The `[[keys.command]]` array is never touched.
pub fn apply_profile(doc: &mut DocumentMut, profile: &DocumentMut) {
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

/// (key, description) for every `[[keys.command]]` entry, in file order.
pub fn custom_commands(doc: &DocumentMut) -> Vec<(String, String)> {
    doc.get("keys")
        .and_then(|i| i.get("command"))
        .and_then(|i| i.as_array_of_tables())
        .map(|aot| {
            aot.iter()
                .filter_map(|t| {
                    let key = t.get("key")?.as_str()?.to_string();
                    let desc = t
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("custom command")
                        .to_string();
                    Some((key, desc))
                })
                .collect()
        })
        .unwrap_or_default()
}
