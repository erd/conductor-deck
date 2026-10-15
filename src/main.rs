#[expect(
    dead_code,
    reason = "actions and subscribe are consumed by the deck UI in #5-#7"
)]
mod herdr;
mod ui;

use std::io::{self, Stdout};
use std::process::ExitCode;

use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use herdr::{HerdrClient, HerdrError, SocketClient};

fn main() -> ExitCode {
    let result = if std::env::args().skip(1).any(|arg| arg == "--probe") {
        SocketClient::from_env().and_then(|client| probe(&client))
    } else {
        deck().map_err(HerdrError::from)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("conductor-deck: {err}");
            ExitCode::FAILURE
        }
    }
}

fn deck() -> io::Result<()> {
    install_panic_hook();
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    run(&mut terminal)
}

/// Prints the plugin context and one `agent get` per pane in the deck's workspace, to check the
/// client by hand against a real herdr.
fn probe(herdr: &impl HerdrClient) -> herdr::Result<()> {
    let context = herdr.plugin_context()?;
    println!("{context:#?}");
    let snapshot = herdr.snapshot()?;
    let workspace = context.workspace_id.as_deref();
    for pane in snapshot
        .panes
        .iter()
        .filter(|pane| workspace.is_none_or(|ws| pane.workspace_id == ws))
    {
        match herdr.agent_get(&pane.pane_id) {
            Ok(agent) => println!(
                "{}\t{}\t{}\t{}",
                pane.pane_id,
                agent.status.as_str(),
                agent.tokens.context.as_deref().unwrap_or("-"),
                agent.tokens.limit.as_deref().unwrap_or("-"),
            ),
            Err(HerdrError::NotFound { .. }) => println!("{}\tno agent", pane.pane_id),
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    loop {
        terminal.draw(ui::draw)?;
        match event::read()? {
            Event::Key(key) if ui::is_quit(&key) => return Ok(()),
            _ => {}
        }
    }
}

/// Puts the terminal into raw mode, alternate screen and mouse capture, and undoes all of it on drop.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn restore_terminal() {
    // Best effort: there is nowhere useful to report a failure while tearing down.
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = disable_raw_mode();
}

/// Restores the terminal before the default hook prints, so a panic message is readable.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}
