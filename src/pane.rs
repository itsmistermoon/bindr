use crate::config;
use std::process::Command;

pub fn open_popup(entrypoint: &str, width: u32, height: u32, focus: bool) {
    let mut cmd = Command::new(config::herdr_bin());
    cmd.args([
        "plugin",
        "pane",
        "open",
        "--plugin",
        &config::plugin_id(),
        "--entrypoint",
        entrypoint,
        "--placement",
        "popup",
        "--width",
        &width.to_string(),
        "--height",
        &height.to_string(),
    ]);
    if !focus {
        cmd.arg("--no-focus");
    }
    let _ = cmd.output();
}
