mod config;
mod keybinds_data;
mod keybinds_view;
mod keys;
mod pane;
mod save;
mod switch;
mod toast;

use std::process::ExitCode;

fn main() -> ExitCode {
    let sub = std::env::args().nth(1);
    let result = match sub.as_deref() {
        Some("switch") => switch::run(),
        Some("save") => save::run(),
        Some("show-keybinds") => {
            pane::open_popup("keybinds", 72, 42, true);
            Ok(())
        }
        Some("toast") => toast::run(),
        Some("keybinds-view") => keybinds_view::run(),
        other => {
            eprintln!("unknown subcommand: {other:?}");
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
