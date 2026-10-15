# conductor-deck

Conductor task visualizer for herdr.

A [herdr](https://herdr.dev) plugin that shows [conductor](https://github.com/erd/conductor) tasks
as stacked cards, docked to the right of the conductor's pane.

## Install

```sh
herdr plugin install erd/conductor-deck
```

Install runs `cargo build --release`, so it needs a Rust toolchain. Requires herdr 0.9.4 or newer
on macOS or Linux.

## Local development

```sh
cargo build --release      # `plugin link` does not run the build step
herdr plugin link .
herdr plugin pane open --plugin conductor-deck --entrypoint deck --placement split --direction right
herdr plugin log list --plugin conductor-deck
```

Rebuild with `cargo build --release` after changes, then reopen the pane. Press `q` or Ctrl-C to
close the deck. Remove the link with `herdr plugin unlink conductor-deck`.

To check the herdr client against a running herdr, run the binary with `--probe` from any herdr
pane. It prints the plugin context and each pane's agent status and token readouts in that
workspace, then exits:

```sh
cargo run -- --probe
```

The Rust toolchain is pinned in `rust-toolchain.toml` to match
[erd/conductor](https://github.com/erd/conductor). CI runs `cargo fmt --check`,
`cargo clippy -- -D warnings` and `cargo test`.

## Keybinding

Bind the `toggle` action in `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+shift+k"
type = "plugin_action"
command = "conductor-deck.toggle"
description = "Toggle the conductor deck"
```

Then run `herdr server reload-config`.
