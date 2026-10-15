mod actions;
mod deck;
mod herdr;
mod repo;
mod ui;

use std::collections::BTreeSet;
use std::io::{self, Stdout};
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use conductor_state::Home;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use jiff::Timestamp;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use deck::watch::{Change, TaskWatcher};
use deck::{CardModel, Deck};
use herdr::types::PluginContext;
use herdr::{Forwarding, HerdrClient, HerdrError, HerdrEvent, SocketClient};
use repo::Location;
use ui::{Action, Body, Input, InputOutcome, Selection, View};

type BoxError = Box<dyn std::error::Error>;

/// How often elapsed times move on screen.
const TICK: Duration = Duration::from_secs(1);
/// How long a toast stays up.
const TOAST: Duration = Duration::from_secs(4);

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
    // A herdr that doesn't answer is shown in the pane rather than ending the deck unseen.
    let location = repo::locate(&herdr, &home);
    install_panic_hook();
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let (tx, rx) = mpsc::channel();
    spawn_input(tx.clone())?;
    match location {
        Ok(Location::Repo { repo_id, name }) => {
            let deck = Deck::open(home, repo_id)?;
            App::new(&herdr, deck, name, tx, rx)?.run(&mut terminal)
        }
        Ok(Location::NoConductor { reason }) => {
            show_until_quit(&mut terminal, &rx, Body::NoConductor { reason: &reason })
        }
        Err(err) => {
            let error = err.to_string();
            show_until_quit(&mut terminal, &rx, Body::HerdrUnreachable { error: &error })
        }
    }
}

/// Shows a screen with nothing to update until the user quits.
fn show_until_quit(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    rx: &mpsc::Receiver<AppEvent>,
    body: Body,
) -> Result<(), BoxError> {
    let mut selection = Selection::default();
    loop {
        let view = View {
            repo: None,
            body,
            now: Timestamp::now(),
            show_hidden: false,
            toast: None,
            input: None,
        };
        terminal.draw(|frame| ui::draw(frame, &view, &mut selection))?;
        match rx.recv() {
            Ok(AppEvent::Input(event)) if ui::action(&event) == Some(Action::Quit) => {
                return Ok(());
            }
            Ok(_) => {}
            Err(_) => return Ok(()),
        }
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
    /// A key press or mouse event.
    Input(Event),
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
                    Event::Key(_) | Event::Mouse(_) => AppEvent::Input(event),
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
    /// The deck's own pane and tab.
    context: PluginContext,
    deck: Deck,
    /// The repo's name, for the header.
    repo: String,
    selection: Selection,
    tx: Sender<AppEvent>,
    rx: mpsc::Receiver<AppEvent>,
    _watcher: TaskWatcher,
    /// The panes subscribed to, and the live subscription.
    subscription: Option<(Vec<String>, Forwarding)>,
    /// The latest herdr or state error, shown until the next herdr refresh succeeds.
    problem: Option<String>,
    /// A message and when it goes away.
    toast: Option<(String, Instant)>,
    /// The message being typed, while `m` is open.
    compose: Option<Compose>,
}

/// A message to a worker, being typed.
struct Compose {
    task_id: String,
    pane: String,
    input: Input,
}

impl<'a> App<'a> {
    fn new(
        herdr: &'a SocketClient,
        deck: Deck,
        repo: String,
        tx: Sender<AppEvent>,
        rx: mpsc::Receiver<AppEvent>,
    ) -> Result<Self, BoxError> {
        let files = tx.clone();
        let watcher = TaskWatcher::start(&deck.repo_dir(), move |change| {
            let _ = files.send(AppEvent::Files(change));
        })?;
        Ok(Self {
            context: herdr.plugin_context()?,
            herdr,
            deck,
            repo,
            selection: Selection::default(),
            tx,
            rx,
            _watcher: watcher,
            subscription: None,
            problem: None,
            toast: None,
            compose: None,
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
            if self
                .toast
                .as_ref()
                .is_some_and(|(_, until)| Instant::now() >= *until)
            {
                self.toast = None;
            }
            let cards = self.deck.cards();
            let task_error =
                (self.deck.errors().iter().next()).map(|(id, err)| format!("task {id}: {err}"));
            let view = View {
                repo: Some(&self.repo),
                body: Body::Cards {
                    cards: &cards,
                    problem: self.problem.as_deref().or(task_error.as_deref()),
                },
                now: self.deck.now(),
                show_hidden: self.deck.show_hidden,
                toast: self.toast.as_ref().map(|(text, _)| text.as_str()),
                input: self.compose.as_ref().map(|compose| compose.input.text()),
            };
            terminal.draw(|frame| ui::draw(frame, &view, &mut self.selection))?;
            let ids: Vec<&str> = cards.iter().map(|card| card.task.id.as_str()).collect();

            let wake = match &self.toast {
                Some((_, until)) => next_tick.min(*until),
                None => next_tick,
            };
            let first = match self
                .rx
                .recv_timeout(wake.saturating_duration_since(Instant::now()))
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
                    AppEvent::Input(event) => {
                        if !self.handle_input(&event, &cards, &ids) {
                            return Ok(());
                        }
                    }
                    AppEvent::Redraw => {}
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

    /// Acts on a key press or click, against the cards as last drawn. Returns `false` to quit.
    fn handle_input(&mut self, event: &Event, cards: &[CardModel], ids: &[&str]) -> bool {
        if let Some(compose) = &mut self.compose {
            if let Event::Key(key) = event {
                match compose.input.handle(key) {
                    InputOutcome::Editing => {}
                    InputOutcome::Cancel => self.compose = None,
                    InputOutcome::Send(text) => {
                        let toast = actions::send_message(
                            self.herdr,
                            &compose.task_id,
                            &compose.pane,
                            &text,
                        );
                        self.compose = None;
                        self.show(toast);
                    }
                }
            }
            return true;
        }
        let Some(action) = ui::action(event) else {
            return true;
        };
        let selected = self.selection.resolve(ids).map(|index| &cards[index]);
        let toast = match action {
            Action::Quit => return false,
            Action::Up => {
                self.selection.move_by(ids, -1);
                None
            }
            Action::Down => {
                self.selection.move_by(ids, 1);
                None
            }
            Action::ToggleHidden => {
                self.deck.show_hidden = !self.deck.show_hidden;
                None
            }
            Action::Focus => selected.and_then(|card| actions::focus_worker(self.herdr, card)),
            Action::Click { row } => self.selection.card_at(row).and_then(|index| {
                self.selection.select_index(ids, index);
                actions::focus_worker(self.herdr, &cards[index])
            }),
            Action::Message => selected.and_then(|card| match actions::start_message(card) {
                Ok(pane) => {
                    self.compose = Some(Compose {
                        task_id: card.task.id.clone(),
                        pane,
                        input: Input::default(),
                    });
                    None
                }
                Err(toast) => Some(toast),
            }),
            Action::OpenPr => {
                selected.and_then(|card| actions::open_pr(card, actions::launch_browser))
            }
            Action::FocusConductor => {
                actions::focus_conductor(self.herdr, &self.deck.repo_dir(), &self.context)
            }
        };
        self.show(toast);
        true
    }

    fn show(&mut self, toast: Option<String>) {
        if let Some(text) = toast {
            self.toast = Some((text, Instant::now() + TOAST));
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
