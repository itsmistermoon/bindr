# bindr

A [Herdr](https://herdr.dev) plugin for switching between named keybinding
profiles, Zellij-preset style. Profiles are TOML snippets holding only the
`[keys]` table; switching one in writes it into your `config.toml` without
touching anything else (theme, UI settings, other plugins' keybinds, etc.).

## Features

- **Switch profiles** — cycles to the next profile alphabetically, or jumps
  to a specific one via `save-as.sh`.
- **Toast popup** — a self-closing popup confirming the switch and listing
  every profile, with the active one marked.
- **Keybinds viewer/editor** — a replica of Herdr's built-in `prefix+?`
  panel, scrollable, with one clickable tab per profile. Browse a profile
  without activating it, preserve the scroll position while comparing tabs,
  edit bindings with `e` (including each profile's own prefix), switch the
  active profile in place with `shift+k` without changing the viewed tab, and
  fuzzy-search entries with `/`. Captures require `enter` to save;
  `backspace` retries and `u` undoes the last saved edit. Editing a prefix
  saves it before any other binding can be edited.
- **Save profile** — snapshot your current `[keys]` table as a new named
  profile.

## Current status

`bindr` is functional but still in active development. The marketplace listing
is intentionally public while the profile editor is being completed.

Currently available:

- Profile switching with an optional confirmation toast.
- A keybind viewer with clickable profile tabs and preserved scroll position.
- In-popup editing of scalar keybinds, including each profile's own `prefix`.
- Safe capture confirmation: `enter` saves, `backspace` retries, `esc` cancels,
  and `u` undoes the last saved edit.
- Editing the active profile updates Herdr immediately; editing another profile
  only changes its TOML file.

Known limitations:

- Profile creation still requires `save-as.sh`.
- Undo currently has one level.
- Some shortcuts can be intercepted by the terminal, SSH/tmux, Herdr, or the
  operating system.
- Custom `[[keys.command]]` bindings remain read-only and outside profiles by
  design.

## Roadmap

1. Create profiles from inside the keybinds popup.
2. Add explicit/manual shortcut entry and collision warnings.
3. Improve undo and profile management (multiple undo levels, rename/delete).
4. Expand interactive testing across local, SSH, and tmux environments.

## Install

```sh
herdr plugin install itsmistermoon/bindr
```

For local development, use `herdr plugin link <path-to-this-repo>` instead
(this skips the `[[build]]` step, so run `cargo build --release` by hand
first).

## Usage

Default keybinds (see `herdr-plugin.toml` for the action IDs):

| Key | Action |
| --- | --- |
| `prefix+shift+k` | Switch to the next profile |
| `prefix+shift+e` | View and edit keybind profiles |

Inside the keybinds popup, use `←/→` or click to browse profiles without
activating them. Press `e` to edit the viewed profile; `Enter` selects a row
and enters key listening mode. `u` undoes the last saved edit.

Profiles live under `HERDR_PLUGIN_CONFIG_DIR/profiles/<name>.toml`. To save
the currently active keybindings as a new profile:

```sh
./save-as.sh <profile-name>
```

## How it works

Each profile file holds only the `[keys]` table (and any `[keys.*]` dotted
subtables). Switching a profile surgically replaces just that table in the
live `config.toml`, leaving `[[keys.command]]` entries (used by this and
other plugins) and every other section untouched.

Built with [`toml_edit`](https://docs.rs/toml_edit) for format-preserving
edits and [`crossterm`](https://docs.rs/crossterm) for the interactive
popups.

## See also

If you use [Ghostty](https://ghostty.org) as your terminal, you can unbind
its own tab/pane/split shortcuts so they pass through to Herdr instead,
avoiding duplicate or conflicting keys:

- [Making Ghostty and Herdr share one keyboard](https://dev.to/oronbz/making-ghostty-and-herdr-share-one-keyboard-4p0g)
- [Ghostty + Herdr keybindings](https://deepakness.com/raw/ghostty-herdr-keybindings/)

For fuzzy-searching keybinds from outside Herdr's own panels, see
[herdr-keybind-search](https://github.com/malone-c/herdr-keybind-search).
