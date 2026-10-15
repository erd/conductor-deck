#!/bin/sh
# Toggles the deck pane beside the pane this action runs from. The deck binary does the work:
# it closes the deck it recorded in this tab, or focuses an unrecorded one, or opens a new one.
# Plugin commands start with a minimal PATH, so the binary is called by its absolute path.
exec "$HERDR_PLUGIN_ROOT/target/release/conductor-deck" toggle
