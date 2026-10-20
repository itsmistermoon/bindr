use std::{fs, path::PathBuf, process::Command};

struct ConfigDir(PathBuf);

impl ConfigDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("bindr-{name}-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn seed(&self) {
        // Without a pending name, save seeds default and exits before reading
        // live configuration or invoking Herdr.
        let output = Command::new(env!("CARGO_BIN_EXE_bindr"))
            .arg("save")
            .env("HERDR_PLUGIN_CONFIG_DIR", &self.0)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("no profile name given"));
    }
}

impl Drop for ConfigDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn seeds_missing_default_with_personal_plugin_keys() {
    let dir = ConfigDir::new("seed-default");
    dir.seed();
    let contents = fs::read_to_string(dir.0.join("profiles/default.toml")).unwrap();
    let profile = contents.parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(profile.len(), 2);
    assert!(profile["keys"].is_table());
    assert_eq!(
        profile["keys"]["rename_tab"].as_str(),
        Some("prefix+shift+t")
    );
    assert_eq!(
        profile["plugin_keys"]["herdr-bar.open"].as_str(),
        Some("cmd+k")
    );
    assert_eq!(
        profile["plugin_keys"]["termscope.open-links"].as_str(),
        Some("cmd+shift+t")
    );
    for (_, binding) in profile["keys"].as_table().unwrap().iter() {
        assert_ne!(binding.as_str(), Some("cmd+k"));
        assert_ne!(binding.as_str(), Some("cmd+shift+t"));
    }
}

#[test]
fn preserves_existing_personal_default_verbatim() {
    let dir = ConfigDir::new("personal-default");
    fs::create_dir_all(dir.0.join("profiles")).unwrap();
    let path = dir.0.join("profiles/default.toml");
    let personal = "# Personal bindings\n[keys]\nrename_tab = 'ctrl+r'\n\n[plugin_keys]\n'example.open' = 'ctrl+p'\n";
    fs::write(&path, personal).unwrap();
    dir.seed();
    dir.seed();
    assert_eq!(fs::read_to_string(path).unwrap(), personal);
}
