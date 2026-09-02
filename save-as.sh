#!/bin/sh
# Save the currently active keybindings as a new (or overwritten) profile.
# Usage: ./save-as.sh <profile-name>
set -e

NAME="$1"
if [ -z "$NAME" ]; then
  echo "usage: save-as.sh <profile-name>" >&2
  exit 1
fi

CFG_DIR=$(herdr plugin config-dir itsmistermoon.bindr)
printf '%s' "$NAME" > "$CFG_DIR/pending-name"
herdr plugin action invoke save-profile --plugin itsmistermoon.bindr
