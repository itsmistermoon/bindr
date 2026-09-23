use crate::{config, keys};
use anyhow::{Result, anyhow, bail};

fn valid_name(name: &str) -> bool {
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

    let doc = keys::load(&config::config_path())?;
    let keys_only = keys::extract_keys_only(&doc);

    let dir = config::profiles_dir();
    std::fs::create_dir_all(&dir)?;
    keys::save(&dir.join(format!("{name}.toml")), &keys_only)?;

    config::write_active_profile(&name)?;
    println!("saved current keybindings as profile '{name}'");
    Ok(())
}
