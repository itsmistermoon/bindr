//! Static mirror of Herdr's built-in prefix+? keybinds panel.
//!
//! Hand-curated from `herdr --default-config` (herdr 0.8.2) plus screenshots
//! of the live panel, because Herdr exposes no API to query its resolved
//! keymap. This can drift if a future Herdr version changes section layout
//! or adds/removes bindings -- if it ever disagrees with the real prefix+?
//! panel, the real panel is correct and this file needs updating.
//!
//! Each row is (config_key, default_value, description). `config_key: None`
//! means the row isn't backed by a single `[keys]` field -- either a fixed
//! navigate-mode summary line, or a panel-only entry like copy mode -- and
//! is shown read-only.

pub struct Row {
    pub config_key: Option<&'static str>,
    pub default: &'static str,
    pub description: &'static str,
}

const fn row(
    config_key: Option<&'static str>,
    default: &'static str,
    description: &'static str,
) -> Row {
    Row {
        config_key,
        default,
        description,
    }
}

pub struct Section {
    pub name: &'static str,
    pub rows: &'static [Row],
}

pub static SECTIONS: &[Section] = &[
    Section {
        name: "global",
        rows: &[
            row(Some("prefix"), "ctrl+b", "prefix mode"),
            row(Some("help"), "prefix+?", "keybinds"),
            row(Some("settings"), "prefix+s", "settings"),
            row(Some("detach"), "prefix+q", "detach"),
            row(Some("reload_config"), "prefix+shift+r", "reload config"),
            row(
                Some("open_notification_target"),
                "prefix+o",
                "open notification target",
            ),
        ],
    },
    Section {
        name: "navigation",
        rows: &[
            row(None, "esc", "back"),
            row(None, "up / down", "workspace list"),
            row(None, "h / j / k / l / left / right", "move focus"),
            row(None, "tab / shift+tab", "cycle pane"),
            row(None, "enter", "open workspace"),
            row(None, "1..9", "switch workspace"),
        ],
    },
    Section {
        name: "workspaces / tabs",
        rows: &[
            row(Some("workspace_picker"), "prefix+w", "workspace navigation"),
            row(Some("goto"), "prefix+g", "session navigator"),
            row(Some("new_workspace"), "prefix+shift+n", "new workspace"),
            row(Some("new_worktree"), "prefix+shift+g", "new worktree"),
            row(Some("open_worktree"), "", "open worktree"),
            row(Some("remove_worktree"), "", "delete worktree checkout"),
            row(
                Some("rename_workspace"),
                "prefix+shift+w",
                "rename workspace",
            ),
            row(Some("close_workspace"), "prefix+shift+d", "close workspace"),
            row(Some("previous_workspace"), "", "previous workspace"),
            row(Some("next_workspace"), "", "next workspace"),
            row(Some("switch_workspace"), "", "switch workspace 1-9"),
            row(Some("previous_agent"), "", "previous agent"),
            row(Some("next_agent"), "", "next agent"),
            row(Some("focus_agent"), "", "focus agent 1-9"),
            row(Some("new_tab"), "prefix+c", "new tab"),
            row(Some("rename_tab"), "prefix+shift+t", "rename tab"),
            row(Some("previous_tab"), "prefix+p", "previous tab"),
            row(Some("next_tab"), "prefix+n", "next tab"),
            row(Some("move_tab_previous"), "", "move tab left"),
            row(Some("move_tab_next"), "", "move tab right"),
            row(Some("switch_tab"), "prefix+1..9", "switch tab 1-9"),
            row(Some("close_tab"), "prefix+shift+x", "close tab"),
        ],
    },
    Section {
        name: "panes",
        rows: &[
            row(Some("split_vertical"), "prefix+v", "split vertical"),
            row(Some("split_horizontal"), "prefix+minus", "split horizontal"),
            row(Some("close_pane"), "prefix+x", "close pane"),
            row(Some("rename_pane"), "prefix+shift+p", "rename pane"),
            row(Some("edit_scrollback"), "prefix+e", "edit scrollback"),
            row(None, "prefix+[", "copy mode"),
            row(Some("zoom"), "prefix+z", "zoom pane"),
            row(Some("resize_mode"), "prefix+r", "resize mode"),
            row(Some("resize_pane_left"), "", "resize pane left"),
            row(Some("resize_pane_down"), "", "resize pane down"),
            row(Some("resize_pane_up"), "", "resize pane up"),
            row(Some("resize_pane_right"), "", "resize pane right"),
            row(Some("toggle_sidebar"), "prefix+b", "toggle sidebar"),
            row(Some("focus_pane_left"), "prefix+h", "focus pane left"),
            row(Some("focus_pane_down"), "prefix+j", "focus pane down"),
            row(Some("focus_pane_up"), "prefix+k", "focus pane up"),
            row(Some("focus_pane_right"), "prefix+l", "focus pane right"),
            row(Some("cycle_pane_next"), "prefix+tab", "cycle pane next"),
            row(
                Some("cycle_pane_previous"),
                "prefix+shift+tab",
                "cycle pane previous",
            ),
            row(Some("last_pane"), "", "last pane"),
        ],
    },
];
