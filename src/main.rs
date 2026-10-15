mod deck;
#[expect(
    dead_code,
    reason = "agent focus/prompt and split ratio are consumed by the deck UI in #5-#7"
)]
mod herdr;
mod repo;
mod ui;

use std::collections::BTreeSet;
use std::io::{self, Stdout};
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use conductor_state::Home;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEvent};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use jiff::Timestamp;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use deck::Deck;
use deck::watch::{Change, TaskWatcher};
use herdr::{Forwarding, HerdrClient, HerdrError, HerdrEvent, SocketClient};
use repo::Location;
use ui::View;

type BoxError = Box<dyn std::error::Error>;

/// How often elapsed times move on screen.
const TICK: Duration = Duration::from_secs(1);

fn main() -> ExitCode {
    let result = if std::env::args().skip(1).any(|arg| arg == "--probe") {
        SocketClient::from_env()
            .and_then(|client| probe(&client))
            .map_err(BoxError::from)
    } else {
        deck()
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("conductor-deck: {err}");
            ExitCode::FAILURE
        }
    }
}

fn deck() -> Result<(), BoxError> {
    let herdr = SocketClient::from_env()?;
    let home = Home::resolve()?;
    let location = repo::locate(&herdr, &home)?;
    install_panic_hook();
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let (tx, rx) = mpsc::channel();
    spawn_input(tx.clone())?;
    match location {
        Location::Repo { repo_id } => {
            let deck = Deck::open(home, repo_id)?;
            App::new(&herdr, deck, tx, rx)?.run(&mut terminal)
        }
        Location::NoConductor { reason } => loop {
            terminal.draw(|frame| ui::draw(frame, &View::NoConductor { reason: &reason }))?;
            match rx.recv() {
                Ok(AppEvent::Key(key)) if ui::is_quit(&key) => return Ok(()),
                Ok(_) => {}
                Err(_) => return Ok(()),
            }
        },
    }
}

/// Prints the plugin context, the conductor repo it maps to, and one `agent get` per pane in
/// the deck's workspace, to check the client by hand against a real herdr.
fn probe(herdr: &impl HerdrClient) -> herdr::Result<()> {
    let context = herdr.plugin_context()?;
    println!("{context:#?}");
    match Home::resolve() {
        Ok(home) => println!("{:?}", repo::locate(herdr, &home)?),
        Err(err) => println!("{err}"),
    }
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

/// Everything the deck's loop waits on, merged into one channel.
enum AppEvent {
    Key(KeyEvent),
    /// A resize or other terminal event that only needs a redraw.
    Redraw,
    Files(Change),
    Herdr(HerdrEvent),
}

fn spawn_input(tx: Sender<AppEvent>) -> io::Result<()> {
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            while let Ok(event) = event::read() {
                let event = match event {
                    Event::Key(key) => AppEvent::Key(key),
                    _ => AppEvent::Redraw,
                };
                if tx.send(event).is_err() {
                    break;
                }
            }
        })?;
    Ok(())
}

struct App<'a> {
    herdr: &'a SocketClient,
    deck: Deck,
    tx: Sender<AppEvent>,
    rx: mpsc::Receiver<AppEvent>,
    _watcher: TaskWatcher,
    /// The panes subscribed to, and the live subscription.
    subscription: Option<(Vec<String>, Forwarding)>,
    /// The latest herdr or state error, shown until the next herdr refresh succeeds.
    problem: Option<String>,
}

impl<'a> App<'a> {
    fn new(
        herdr: &'a SocketClient,
        deck: Deck,
        tx: Sender<AppEvent>,
        rx: mpsc::Receiver<AppEvent>,
    ) -> Result<Self, BoxError> {
        let files = tx.clone();
        let watcher = TaskWatcher::start(&deck.repo_dir(), move |change| {
            let _ = files.send(AppEvent::Files(change));
        })?;
        Ok(Self {
            herdr,
            deck,
            tx,
            rx,
            _watcher: watcher,
            subscription: None,
            problem: None,
        })
    }

    fn run(mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<(), BoxError> {
        let mut next_tick = Instant::now();
        loop {
            self.resubscribe();
            if Instant::now() >= next_tick {
                self.deck.tick(Timestamp::now());
                let refreshed = self.deck.refresh_stale_agents(self.herdr, Instant::now());
                self.problem = refreshed.err().map(|err| err.to_string());
                next_tick = Instant::now() + TICK;
            }
            let cards = self.deck.cards();
            let task_error =
                (self.deck.errors().iter().next()).map(|(id, err)| format!("task {id}: {err}"));
            let view = View::Cards {
                cards: &cards,
                problem: self.problem.as_deref().or(task_error.as_deref()),
            };
            terminal.draw(|frame| ui::draw(frame, &view))?;

            let first = match self
                .rx
                .recv_timeout(next_tick.saturating_duration_since(Instant::now()))
            {
                Ok(event) => Some(event),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            };
            // Drain everything pending so a burst of writes reloads each task once.
            let mut changed = BTreeSet::new();
            let mut rescan = false;
            let pending: Vec<_> = first.into_iter().chain(self.rx.try_iter()).collect();
            for event in pending {
                match event {
                    AppEvent::Key(key) if ui::is_quit(&key) => return Ok(()),
                    AppEvent::Key(_) | AppEvent::Redraw => {}
                    AppEvent::Files(Change::Task(id)) => {
                        changed.insert(id);
                    }
                    AppEvent::Files(Change::Rescan) => rescan = true,
                    AppEvent::Herdr(event) => {
                        let applied = self.deck.apply_herdr(self.herdr, event, Instant::now());
                        self.note(applied);
                    }
                }
            }
            if rescan {
                let rescanned = self.deck.rescan();
                self.note(rescanned);
            } else {
                for id in changed {
                    self.deck.task_changed(&id);
                }
            }
        }
    }

    /// herdr only reports agent status for the panes named when subscribing, so subscribe
    /// again whenever the set of task panes changes.
    fn resubscribe(&mut self) {
        let panes = self.deck.watched_panes();
        if self
            .subscription
            .as_ref()
            .is_some_and(|(watched, _)| *watched == panes)
        {
            return;
        }
        // Stop the old stream before opening the new one.
        self.subscription = None;
        let subscribed = self.herdr.subscribe(&panes).and_then(|subscription| {
            subscription
                .forward(self.tx.clone(), AppEvent::Herdr)
                .map_err(HerdrError::from)
        });
        match subscribed {
            Ok(forwarding) => self.subscription = Some((panes, forwarding)),
            Err(err) => self.problem = Some(err.to_string()),
        }
    }

    fn note<E: std::fmt::Display>(&mut self, result: Result<(), E>) {
        if let Err(err) = result {
            self.problem = Some(err.to_string());
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
