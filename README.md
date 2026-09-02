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
- **Keybinds viewer** — a read-only replica of Herdr's built-in `prefix+?`
  panel, scrollable, that also lets you switch profiles in place with
  `shift+k` without losing your scroll position, and fuzzy-search entries
  with `/`.
- **Save profile** — snapshot your current `[keys]` table as a new named
  profile.

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
| `prefix+shift+e` | View all keybinds |

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
