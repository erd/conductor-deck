//! The `decide` popup: the decision modal for one task, opened by `d` in a herdr popup centered
//! over the whole window. The deck passes the task through the environment, since a popup gets no
//! `HERDR_PANE_ID`. The popup closes when this process exits.

use std::io;
use std::path::Path;
use std::process::Output;

use crossterm::event::{KeyEvent, KeyEventKind};

use crate::actions::{self, Answered};
use crate::deck::CardModel;
use crate::herdr::{HerdrClient, HerdrError};
use crate::ui::{DecisionModal, DecisionOutcome};

/// The popup's pane entrypoint id in `herdr-plugin.toml`.
pub const ENTRYPOINT: &str = "decide";

const TASK: &str = "CONDUCTOR_DECK_TASK";
const REPO: &str = "CONDUCTOR_DECK_REPO";
/// The worker's pane, to tell it once the answer is recorded. Unset when it has none.
const PANE: &str = "CONDUCTOR_DECK_PANE";

/// The task whose decisions the popup answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub task_id: String,
    pub repo_id: String,
    pub pane: Option<String>,
}

impl Request {
    /// The environment that carries the request to the popup.
    pub fn env(&self) -> Vec<(String, String)> {
        let mut env = vec![
            (TASK.to_owned(), self.task_id.clone()),
            (REPO.to_owned(), self.repo_id.clone()),
        ];
        if let Some(pane) = &self.pane {
            env.push((PANE.to_owned(), pane.clone()));
        }
        env
    }

    pub fn from_env() -> Result<Self, HerdrError> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// Reads the request with `var`, which looks up one environment variable.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self, HerdrError> {
        let var = |name| var(name).filter(|value| !value.is_empty());
        Ok(Self {
            task_id: var(TASK).ok_or(HerdrError::MissingEnv(TASK))?,
            repo_id: var(REPO).ok_or(HerdrError::MissingEnv(REPO))?,
            pane: var(PANE),
        })
    }
}

/// What the popup does after a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Stay,
    /// Exit, which closes the popup.
    Exit,
}

/// The popup's state: the modal while there is something to answer, and a message to show.
#[derive(Debug)]
pub struct Popup {
    repo_id: String,
    modal: Option<DecisionModal>,
    message: Option<String>,
}

impl Popup {
    /// The popup for `request`, given its task's card if the task still exists.
    pub fn new(request: Request, card: Option<CardModel>) -> Self {
        let decisions = card.map(|card| card.open_decisions).unwrap_or_default();
        let modal = DecisionModal::new(request.task_id.clone(), request.pane, decisions);
        // Answered elsewhere, or the task is gone, since `d` was pressed.
        let message = modal
            .is_none()
            .then(|| format!("{}: no open decisions", request.task_id));
        Self {
            repo_id: request.repo_id,
            modal,
            message,
        }
    }

    pub fn modal(&self) -> Option<&DecisionModal> {
        self.modal.as_ref()
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Passes `key` to the modal. An answer is recorded with `run` and `bin`, as the deck's own
    /// modal does. A clean answer or a cancel exits. A failed answer stays open to say why. An
    /// answer the worker couldn't be told about says so, then exits on the next key. Without a
    /// modal, any key exits.
    pub fn handle(
        &mut self,
        key: &KeyEvent,
        herdr: &impl HerdrClient,
        run: impl FnOnce(&Path, &[String]) -> io::Result<Output>,
        bin: Option<&Path>,
    ) -> Step {
        if key.kind == KeyEventKind::Release {
            return Step::Stay;
        }
        let Some(modal) = &mut self.modal else {
            return Step::Exit;
        };
        let answer = match modal.handle(key) {
            DecisionOutcome::Open => return Step::Stay,
            DecisionOutcome::Cancel => return Step::Exit,
            DecisionOutcome::Submit(answer) => answer,
        };
        let answered = actions::submit_answer(
            herdr,
            run,
            bin,
            &self.repo_id,
            modal.task_id(),
            modal.pane(),
            &answer,
        );
        match answered {
            Answered::Told(_) => Step::Exit,
            Answered::Failed(message) => {
                self.message = Some(message);
                Step::Stay
            }
            Answered::Untold(message) => {
                self.modal = None;
                self.message = Some(message);
                Step::Stay
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use conductor_state::Phase;
    use crossterm::event::{KeyCode, KeyModifiers};

    use super::*;
    use crate::actions::testing::{Runs, runner};
    use crate::deck::Deck;
    use crate::deck::testing::{REPO_ID, TempRepo, decision, task};
    use crate::herdr::fake::{Action, FakeHerdr};
    use crate::herdr::types::{PluginContext, Snapshot};

    const BIN: &str = "/home/u/.cargo/bin/conductor";

    fn fake() -> FakeHerdr {
        let context = PluginContext::from_parts(None, Some("w1:p9".into()), None, None).unwrap();
        FakeHerdr::new(context, Snapshot::default())
    }

    fn request() -> Request {
        Request {
            task_id: "t".into(),
            repo_id: REPO_ID.into(),
            pane: Some("w2:p1".into()),
        }
    }

    /// Task `t`'s card, with an open `db-engine` decision recommending SQLite.
    fn card() -> CardModel {
        let repo = TempRepo::new();
        let mut task = task("t", Phase::NeedsDecision);
        task.herdr_pane_id = Some("w2:p1".into());
        repo.write_task(&task);
        let mut card = Deck::open(repo.home.clone(), REPO_ID.into())
            .unwrap()
            .card("t")
            .unwrap();
        let mut engine = decision(
            "db-engine",
            &[("pg", "Postgres", None), ("sqlite", "SQLite", None)],
        );
        engine.recommend = Some("sqlite".into());
        card.open_decisions = vec![engine];
        card
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press(
        popup: &mut Popup,
        herdr: &FakeHerdr,
        runs: &Runs,
        code: i32,
        keys: &[KeyCode],
    ) -> Step {
        let mut step = Step::Stay;
        for &k in keys {
            step = popup.handle(
                &key(k),
                herdr,
                runner(runs, code, "boom\n"),
                Some(Path::new(BIN)),
            );
        }
        step
    }

    #[test]
    fn request_round_trips_through_the_env() {
        let vars = |request: &Request| {
            let env = request.env();
            Request::from_vars(|name| {
                env.iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, value)| value.clone())
            })
        };
        assert_eq!(vars(&request()).unwrap(), request());
        let paneless = Request {
            pane: None,
            ..request()
        };
        assert_eq!(paneless.env().len(), 2);
        assert_eq!(vars(&paneless).unwrap(), paneless);
        assert!(matches!(
            Request::from_vars(|_| None),
            Err(HerdrError::MissingEnv(TASK))
        ));
    }

    #[test]
    fn answers_like_the_deck_and_exits() {
        let keys = [
            KeyCode::Tab,
            KeyCode::Char('o'),
            KeyCode::Char('k'),
            KeyCode::Enter,
        ];

        // The deck's own modal, answered through `submit_answer`.
        let deck_herdr = fake();
        let deck_runs = RefCell::new(Vec::new());
        let mut modal = actions::start_decision(&card()).unwrap();
        let mut outcome = DecisionOutcome::Open;
        for k in keys {
            outcome = modal.handle(&key(k));
        }
        let DecisionOutcome::Submit(answer) = outcome else {
            panic!("expected an answer, got {outcome:?}");
        };
        actions::submit_answer(
            &deck_herdr,
            runner(&deck_runs, 0, ""),
            Some(Path::new(BIN)),
            REPO_ID,
            modal.task_id(),
            modal.pane(),
            &answer,
        );

        // The popup, with the same keys.
        let herdr = fake();
        let runs = RefCell::new(Vec::new());
        let mut popup = Popup::new(request(), Some(card()));
        assert_eq!(press(&mut popup, &herdr, &runs, 0, &keys), Step::Exit);

        let args = [
            "decision",
            "answer",
            "db-engine",
            "sqlite",
            "--note",
            "ok",
            "--by",
            "deck",
            "--task",
            "t",
            "--repo",
            REPO_ID,
        ]
        .map(ToOwned::to_owned)
        .to_vec();
        assert_eq!(*runs.borrow(), [(PathBuf::from(BIN), args)]);
        assert_eq!(*runs.borrow(), *deck_runs.borrow());
        assert_eq!(herdr.actions(), deck_herdr.actions());
        assert!(matches!(&herdr.actions()[..], [Action::Prompt { pane, .. }] if pane == "w2:p1"));
    }

    #[test]
    fn esc_exits_without_answering() {
        let herdr = fake();
        let runs = RefCell::new(Vec::new());
        let mut popup = Popup::new(request(), Some(card()));
        assert_eq!(
            press(&mut popup, &herdr, &runs, 0, &[KeyCode::Down]),
            Step::Stay
        );
        assert_eq!(
            press(&mut popup, &herdr, &runs, 0, &[KeyCode::Esc]),
            Step::Exit
        );
        assert!(runs.borrow().is_empty());
        assert_eq!(herdr.actions(), []);
    }

    #[test]
    fn a_failed_answer_stays_open_to_say_why() {
        let herdr = fake();
        let runs = RefCell::new(Vec::new());
        let mut popup = Popup::new(request(), Some(card()));
        assert_eq!(
            press(&mut popup, &herdr, &runs, 1, &[KeyCode::Enter]),
            Step::Stay
        );
        assert_eq!(popup.message(), Some("answer failed: boom"));
        assert!(popup.modal().is_some());
        // It can be answered again.
        assert_eq!(
            press(&mut popup, &herdr, &runs, 0, &[KeyCode::Enter]),
            Step::Exit
        );
        assert_eq!(runs.borrow().len(), 2);
    }

    #[test]
    fn an_untold_answer_says_so_then_exits_on_any_key() {
        let herdr = fake();
        herdr.block("w2:p1");
        let runs = RefCell::new(Vec::new());
        let mut popup = Popup::new(request(), Some(card()));
        assert_eq!(
            press(&mut popup, &herdr, &runs, 0, &[KeyCode::Enter]),
            Step::Stay
        );
        assert!(popup.modal().is_none());
        assert_eq!(
            popup.message(),
            Some("answered db-engine; worker is waiting on a prompt, so it wasn't told")
        );
        assert_eq!(
            press(&mut popup, &herdr, &runs, 0, &[KeyCode::Char('x')]),
            Step::Exit
        );
        assert_eq!(runs.borrow().len(), 1);
    }

    #[test]
    fn nothing_left_to_answer_says_so() {
        let herdr = fake();
        let runs = RefCell::new(Vec::new());
        let mut answered = card();
        answered.open_decisions.clear();
        for card in [None, Some(answered)] {
            let mut popup = Popup::new(request(), card);
            assert!(popup.modal().is_none());
            assert_eq!(popup.message(), Some("t: no open decisions"));
            assert_eq!(
                press(&mut popup, &herdr, &runs, 0, &[KeyCode::Enter]),
                Step::Exit
            );
        }
        assert!(runs.borrow().is_empty());
    }
}
