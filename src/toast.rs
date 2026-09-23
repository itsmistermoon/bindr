//! Self-closing popup: lists every profile, marking the active one.
//! Simulates a toast without using Herdr's own notification surface.

use crate::config;
use anyhow::Result;
use crossterm::{cursor, execute, terminal};
use std::io::{Write, stdout};
use std::thread::sleep;
use std::time::Duration;

const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";
const LEFT_PAD: &str = "  ";

pub fn run() -> Result<()> {
    // Raw mode + hidden cursor: this pane never reads input, but without
    // this a stray keystroke would be echoed as garbage into the popup.
    terminal::enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, cursor::Hide)?;

    let active = config::read_active_profile();
    let profiles = config::list_profiles()?;

    let mut lines = vec![String::new()];
    for name in &profiles {
        let row = if Some(name.as_str()) == active.as_deref() {
            format!("{CYAN}\u{25cf}{RESET} {BOLD}{name}{RESET}")
        } else {
            format!("  {name}")
        };
        lines.push(format!("{LEFT_PAD}{row}"));
    }
    lines.push(String::new());

    // No trailing newline after the last (blank) line: in a terminal
    // exactly `lines` tall, one more line scrolls the first line out of
    // view.
    write!(out, "{}", lines.join("\r\n"))?;
    out.flush()?;

    sleep(Duration::from_millis(1500));

    let _ = execute!(out, cursor::Show);
    let _ = terminal::disable_raw_mode();
    Ok(())
}
