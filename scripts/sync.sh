#!/bin/sh
# Copies mods/conductor-deck into the Herdr app's mods folder. Herdr doesn't hot-reload a
# symlinked mod, so it gets real files. With --watch, copies again on every change.
#
#   scripts/sync.sh [--watch]
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
src="$here/mods/conductor-deck"
dst="${HERDR_APP_HOME:-$HOME/.config/herdr-app}/mods/conductor-deck"

sync_once() {
  [ -L "$dst" ] && rm "$dst"
  mkdir -p "$dst"
  # The app writes gpui-kit.d.ts and jsconfig.json into the mod's folder itself.
  rsync -a --delete --exclude gpui-kit.d.ts --exclude jsconfig.json --exclude '.*' "$src/" "$dst/"
}

sync_once
echo "synced -> $dst"
[ "${1:-}" = "--watch" ] || exit 0
last=""
while :; do
  now=$(find "$src" -type f ! -name gpui-kit.d.ts ! -name jsconfig.json -exec shasum {} + | sort | shasum)
  if [ "$now" != "$last" ]; then
    [ -n "$last" ] && sync_once && echo "synced $(date +%T)"
    last=$now
  fi
  sleep 1
done
