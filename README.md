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

Rebuild with `cargo build --release` after changes, then reopen the pane. Remove the link with
`herdr plugin unlink conductor-deck`.

The UI snapshot tests use [insta](https://insta.rs). After a deliberate change to the screen, run
`cargo insta review` (or `INSTA_UPDATE=always cargo test`) and check the diffs in
`src/ui/snapshots/`.

## Keys

| Key | Does |
| --- | --- |
| `j`/`k`, `↓`/`↑`, mouse wheel | Move the selection |
| `Enter`, click | Jump to the card's worker |
| `m` | Message the card's worker: type a line, `Enter` sends, `Esc` cancels |
| `d` | Answer the card's open decisions, oldest first (see below) |
| `o` | Open the card's PR in the browser |
| `c` | Jump to the conductor |
| `L` | Show or hide torn-down and abandoned tasks |
| `q`, Ctrl-C | Close the deck |

In the decision modal, `↑`/`↓` (or `j`/`k`) choose an option, `Tab` moves to the note, `n`/`p`
show the next or previous open decision, `Enter` answers and `Esc` cancels. A decision that allows
free text has a last `other…` row, which answers with what's typed in the note. The deck records the
answer with `conductor decision answer … --by deck`, then prompts the worker to continue. It runs
`CONDUCTOR_BIN` if set, else `~/.cargo/bin/conductor`, since plugin panes start with a minimal
`PATH`.

To check the herdr client against a running herdr, run the binary with `--probe` from any herdr
pane. It prints the plugin context, the conductor repo that workspace maps to (or why there is
none), and each pane's agent status and token readouts in that workspace, then exits:

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

Run from a pane, the action:

- closes the deck in that pane's tab, if there is one;
- otherwise opens a deck to the right of that pane.

The action only closes decks the plugin knows about. Every deck records its own pane id in
`$HERDR_PLUGIN_STATE_DIR/panes` when it starts, so this includes decks opened by conductor's
SessionStart hook. A pane titled `conductor` that the plugin didn't record is focused, never
closed, and the action doesn't open a second deck beside it.

## Width

The deck holds itself at `deck_width` columns, 48 by default. Set it in conductor's
`~/.config/conductor/config.toml`, globally or for one repo:

```toml
deck_width = 56

[repos."<repo-id>"]
deck_width = 40
```

herdr sizes splits by ratio, so the deck rewrites its split's ratio 200 ms after the layout or
the terminal stops changing. When you drag the divider yourself, the deck keeps your width until it
closes. It always leaves at least 20 columns beside itself, and herdr keeps each side of a split
between 10% and 90% of its width.
