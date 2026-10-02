//! Focused, short-lived popup: lists every profile, marking the active one,
//! and lets shift+k cycle profiles again before it closes.

use crate::config;
use crate::switch;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{cursor, execute, terminal};
use std::io::{Write, stdout};
use std::time::{Duration, Instant};

const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const BG_GREEN: &str = "\x1b[42m";
const FG_BLACK: &str = "\x1b[30m";
const RESET: &str = "\x1b[0m";
const LEFT_PAD: &str = "  ";

pub fn run() -> Result<()> {
    terminal::enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, cursor::Hide)?;

    let result = run_toast(&mut out);

    let _ = execute!(out, cursor::Show);
    let _ = terminal::disable_raw_mode();
    result
}

fn run_toast(out: &mut impl Write) -> Result<()> {
    let mut active = config::read_active_profile();
    let mut profiles = config::list_profiles()?;
    let mut expires_at = Instant::now() + Duration::from_secs(2);

    loop {
        render(out, &profiles, active.as_deref())?;
        let now = Instant::now();
        if now >= expires_at {
            break;
        }

        if event::poll(expires_at.saturating_duration_since(now))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind == KeyEventKind::Press && is_shift_k(&key) {
                switch::switch_to_next()?;
                active = config::read_active_profile();
                profiles = config::list_profiles()?;
                expires_at = Instant::now() + Duration::from_secs(2);
            } else {
                // The short-lived popup should not keep consuming input
                // after the user presses a different key.
                break;
            }
        }
    }

    Ok(())
}

fn is_shift_k(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('K'))
        || (matches!(key.code, KeyCode::Char('k')) && key.modifiers.contains(KeyModifiers::SHIFT))
}

fn render(out: &mut impl Write, profiles: &[String], active: Option<&str>) -> Result<()> {
    execute!(
        out,
        terminal::Clear(terminal::ClearType::All),
        cursor::MoveTo(0, 0)
    )?;
    let mut lines = vec![String::new()];
    for name in profiles {
        let row = if Some(name.as_str()) == active {
            format!("{GREEN}{BG_GREEN}{FG_BLACK}{BOLD} {name} {RESET}{GREEN}{RESET}")
        } else {
            format!("  {name}")
        };
        lines.push(format!("{LEFT_PAD}{row}"));
    }
    lines.push(String::new());

    // No trailing newline after the last (blank) line: in a terminal
    // exactly `lines` tall, one more line scrolls the first line out of view.
    write!(out, "{}", lines.join("\r\n"))?;
    out.flush()?;
    Ok(())
}
