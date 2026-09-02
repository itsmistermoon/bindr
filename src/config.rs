use anyhow::{bail, Context, Result};
use std::env;
use std::fs;
use std::path::PathBuf;

pub fn config_path() -> PathBuf {
    env::var("HERDR_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = env::var("HOME").expect("HOME not set");
            PathBuf::from(home).join(".config/herdr/config.toml")
        })
}

pub fn plugin_id() -> String {
    env::var("HERDR_PLUGIN_ID").expect("HERDR_PLUGIN_ID not set")
}

pub fn herdr_bin() -> String {
    env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string())
}

pub fn profiles_dir() -> PathBuf {
    let config_dir =
        env::var("HERDR_PLUGIN_CONFIG_DIR").expect("HERDR_PLUGIN_CONFIG_DIR not set");
    PathBuf::from(config_dir).join("profiles")
}

pub fn state_file() -> PathBuf {
    let state_dir = env::var("HERDR_PLUGIN_STATE_DIR").expect("HERDR_PLUGIN_STATE_DIR not set");
    PathBuf::from(state_dir).join("active-profile")
}

pub fn pending_name_file() -> PathBuf {
    let config_dir =
        env::var("HERDR_PLUGIN_CONFIG_DIR").expect("HERDR_PLUGIN_CONFIG_DIR not set");
    PathBuf::from(config_dir).join("pending-name")
}

/// Read and consume the pending-name file, if present.
pub fn take_pending_name() -> Result<Option<String>> {
    let path = pending_name_file();
    let name = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("reading pending-name file"),
    };
    fs::remove_file(&path).context("removing pending-name file")?;
    let name = name.trim().to_string();
    Ok(if name.is_empty() { None } else { Some(name) })
}

pub fn list_profiles() -> Result<Vec<String>> {
    let dir = profiles_dir();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut names: Vec<String> = fs::read_dir(&dir)
        .with_context(|| format!("reading profiles dir {dir:?}"))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            name.strip_suffix(".toml").map(|s| s.to_string())
        })
        .collect();
    names.sort();
    Ok(names)
}

pub fn read_active_profile() -> Option<String> {
    let text = fs::read_to_string(state_file()).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn write_active_profile(name: &str) -> Result<()> {
    let path = state_file();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{name}\n"))?;
    Ok(())
}

/// Pick the switch target: an explicit pending-name if present, otherwise
/// the profile alphabetically after the currently active one (wrapping).
pub fn pick_target(profiles: &[String]) -> Result<String> {
    if let Some(explicit) = take_pending_name()? {
        if !profiles.iter().any(|p| p == &explicit) {
            bail!(
                "profile '{explicit}' not found in {:?}",
                profiles_dir()
            );
        }
        return Ok(explicit);
    }

    let current = read_active_profile();
    let idx = current
        .as_deref()
        .and_then(|c| profiles.iter().position(|p| p == c));
    Ok(match idx {
        Some(i) => profiles[(i + 1) % profiles.len()].clone(),
        None => profiles[0].clone(),
    })
}
