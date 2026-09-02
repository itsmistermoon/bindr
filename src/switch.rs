//! Switch the active keybind profile.
//!
//! Target profile selection:
//!   - A pending-name file (see config::take_pending_name) picks that
//!     profile explicitly, if present.
//!   - Otherwise cycles alphabetically to the profile after the currently
//!     active one (wrapping around), like Zellij keybinding presets.

use crate::{config, keys, pane};
use anyhow::{bail, Result};
use std::process::Command;

/// Apply the next (or pending-name) profile to config.toml and reload.
/// Shared by the `switch` subcommand and the in-popup shift+K shortcut in
/// keybinds_view; callers decide separately whether to show a toast.
pub fn switch_to_next() -> Result<String> {
    let profiles = config::list_profiles()?;
    if profiles.is_empty() {
        bail!("no profiles found in {:?}", config::profiles_dir());
    }
    let target = config::pick_target(&profiles)?;
    let profile_path = config::profiles_dir().join(format!("{target}.toml"));
    let profile_doc = keys::load(&profile_path)?;

    let cfg_path = config::config_path();
    let mut doc = keys::load(&cfg_path)?;
    keys::apply_profile(&mut doc, &profile_doc);
    keys::save(&cfg_path, &doc)?;

    config::write_active_profile(&target)?;

    let output = Command::new(config::herdr_bin())
        .args(["server", "reload-config"])
        .output()?;
    if !output.status.success() {
        bail!(
            "switched to profile '{target}' but reload-config failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(target)
}

pub fn run() -> Result<()> {
    let profile_count = config::list_profiles()?.len() as u32;
    let target = switch_to_next()?;
    pane::open_popup("toast", 30, profile_count + 4, false);
    println!("keybind profile switched to '{target}'");
    Ok(())
}
