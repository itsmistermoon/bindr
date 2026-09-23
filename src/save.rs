use crate::{config, keys};
use anyhow::{Result, anyhow, bail};

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn run() -> Result<()> {
    config::ensure_default_profile()?;
    let name = config::take_pending_name()?.ok_or_else(|| {
        anyhow!("no profile name given (write it to the pending-name file first)")
    })?;
    if !valid_name(&name) {
        bail!("invalid profile name '{name}': use only [a-zA-Z0-9_-]");
    }
    if name == "default" {
        bail!("profile 'default' is reserved and cannot be overwritten");
    }

    let keys_only = snapshot_live()?;

    let dir = config::profiles_dir();
    std::fs::create_dir_all(&dir)?;
    keys::save(&dir.join(format!("{name}.toml")), &keys_only)?;

    config::write_active_profile(&name)?;
    println!("saved current keybindings as profile '{name}'");
    Ok(())
}

/// The live keybindings as a profile: the `[keys]` table plus, for every
/// plugin command a profile currently rebinds, its live binding in
/// `[plugin_keys]`. Commands still on the plugin's own binding are left out
/// so they keep following the plugin.
fn snapshot_live() -> Result<toml_edit::DocumentMut> {
    let live = keys::load(&config::config_path())?;
    let mut profile = keys::extract_keys_only(&live);
    let displaced = config::load_displaced_plugin_keys()?;
    for command in keys::custom_commands(&live) {
        if let Some(id) = command.id
            && displaced.contains_key(&id)
        {
            keys::Target::Command(id).set(&mut profile, &command.key);
        }
    }
    Ok(profile)
}
