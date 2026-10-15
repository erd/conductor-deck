//! The card model: every task of one repo, folded from conductor's state files and joined
//! with herdr's live agent data.

pub mod card;
mod task;
#[cfg(test)]
pub mod testing;
pub mod watch;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use conductor_state::{Home, Phase, task_ids};
use jiff::Timestamp;

use crate::herdr::types::{Agent, Pane};
use crate::herdr::{self, HerdrClient, HerdrError, HerdrEvent};
pub use card::{AgentInfo, CardModel};
use task::TaskState;

/// How old herdr's token readouts may get before the deck asks again. Status changes are
/// pushed, so this only bounds how stale context % and the usage limit can be.
pub const AGENT_REFRESH: Duration = Duration::from_secs(10);

pub struct Deck {
    home: Home,
    repo_id: String,
    tasks: BTreeMap<String, TaskState>,
    /// The latest read error per task id. The task keeps its last good state.
    errors: BTreeMap<String, String>,
    /// herdr's agent per pane id.
    agents: HashMap<String, AgentEntry>,
    now: Timestamp,
    /// Show torn-down and abandoned tasks too.
    pub show_hidden: bool,
}

struct AgentEntry {
    /// `None` when the pane has no agent, or is gone.
    info: Option<AgentInfo>,
    workspace_id: Option<String>,
    fetched: Instant,
}

impl Deck {
    /// Loads every task under `repos/<repo_id>/tasks/`.
    pub fn open(home: Home, repo_id: String) -> conductor_state::Result<Self> {
        let mut deck = Self {
            home,
            repo_id,
            tasks: BTreeMap::new(),
            errors: BTreeMap::new(),
            agents: HashMap::new(),
            now: Timestamp::now(),
            show_hidden: false,
        };
        deck.rescan()?;
        Ok(deck)
    }

    /// Re-reads the task list and every task, for when the file watch may have missed
    /// changes.
    pub fn rescan(&mut self) -> conductor_state::Result<()> {
        let ids = task_ids(&self.home.repo_dir(&self.repo_id))?;
        self.tasks.retain(|id, _| ids.contains(id));
        self.errors.retain(|id, _| ids.contains(id));
        for id in &ids {
            self.task_changed(id);
        }
        Ok(())
    }

    /// Reloads one task after its directory changed: picks up a new task, folds in new
    /// events, and drops a task whose `task.json` is gone.
    pub fn task_changed(&mut self, id: &str) {
        let dir = self.home.task_dir(&self.repo_id, id);
        let present = match self.tasks.get_mut(id) {
            Some(state) => state.refresh(&dir),
            None => TaskState::load(&dir).map(|state| {
                let found = state.is_some();
                if let Some(state) = state {
                    self.tasks.insert(id.to_owned(), state);
                }
                found
            }),
        };
        match present {
            Ok(true) => {
                self.errors.remove(id);
            }
            Ok(false) => {
                self.tasks.remove(id);
                self.errors.remove(id);
            }
            Err(err) => {
                self.errors.insert(id.to_owned(), err.to_string());
            }
        }
    }

    pub fn repo_dir(&self) -> std::path::PathBuf {
        self.home.repo_dir(&self.repo_id)
    }

    pub fn errors(&self) -> &BTreeMap<String, String> {
        &self.errors
    }

    /// The panes of live tasks, whose agent status the deck subscribes to.
    pub fn watched_panes(&self) -> Vec<String> {
        let panes: BTreeSet<&str> = self
            .tasks
            .values()
            .filter(|state| !hidden_by_default(state.task.phase))
            .filter_map(pane_of)
            .collect();
        panes.into_iter().map(str::to_owned).collect()
    }

    /// Updates herdr's agent data from one subscription event.
    pub fn apply_herdr(
        &mut self,
        herdr: &impl HerdrClient,
        event: HerdrEvent,
        now: Instant,
    ) -> herdr::Result<()> {
        match event {
            HerdrEvent::Resync(snapshot) => {
                let mut agents: HashMap<String, Agent> = snapshot
                    .agents
                    .into_iter()
                    .map(|agent| (agent.pane_id.clone(), agent))
                    .collect();
                self.agents = self
                    .watched_panes()
                    .into_iter()
                    .map(|pane| {
                        let entry = AgentEntry::from_agent(agents.remove(&pane), now);
                        (pane, entry)
                    })
                    .collect();
            }
            HerdrEvent::AgentStatusChanged {
                pane_id, status, ..
            } => {
                if let Some(info) = self.agents.get_mut(&pane_id).and_then(|e| e.info.as_mut()) {
                    info.status = status;
                }
                // The status is pushed; the token readouts have to be asked for.
                if self.watched_panes().contains(&pane_id) {
                    return self.fetch(herdr, &pane_id, now);
                }
            }
            HerdrEvent::PaneUpdated(pane) => {
                if self.watched_panes().contains(&pane.pane_id) {
                    let pane_id = pane.pane_id.clone();
                    self.agents
                        .insert(pane_id, AgentEntry::from_pane(pane, now));
                }
            }
            HerdrEvent::PaneClosed { pane_id, .. } => {
                if let Some(entry) = self.agents.get_mut(&pane_id) {
                    entry.info = None;
                }
            }
            HerdrEvent::WorkspaceClosed { workspace_id } => {
                for entry in self.agents.values_mut() {
                    if entry.workspace_id.as_deref() == Some(&workspace_id) {
                        entry.info = None;
                    }
                }
            }
            HerdrEvent::LayoutUpdated(_) | HerdrEvent::Disconnected => {}
        }
        Ok(())
    }

    /// Asks herdr again for each watched agent last fetched [`AGENT_REFRESH`] or more ago.
    /// Returns the first error, after trying every pane.
    pub fn refresh_stale_agents(
        &mut self,
        herdr: &impl HerdrClient,
        now: Instant,
    ) -> herdr::Result<()> {
        let watched = self.watched_panes();
        self.agents.retain(|pane, _| watched.contains(pane));
        let mut result = Ok(());
        for pane in watched {
            let stale = self
                .agents
                .get(&pane)
                .is_none_or(|entry| now.duration_since(entry.fetched) >= AGENT_REFRESH);
            if stale {
                result = result.and(self.fetch(herdr, &pane, now));
            }
        }
        result
    }

    /// Moves the clock that elapsed times are measured against. Reads nothing.
    pub fn tick(&mut self, now: Timestamp) {
        self.now = now;
    }

    pub fn now(&self) -> Timestamp {
        self.now
    }

    /// The cards to show, sorted.
    pub fn cards(&self) -> Vec<CardModel> {
        let mut cards: Vec<CardModel> = self
            .tasks
            .values()
            .filter(|state| self.show_hidden || !hidden_by_default(state.task.phase))
            .map(|state| {
                let agent = pane_of(state)
                    .and_then(|pane| self.agents.get(pane))
                    .and_then(|entry| entry.info.as_ref());
                CardModel::new(state, agent)
            })
            .collect();
        card::sort(&mut cards);
        cards
    }

    fn fetch(&mut self, herdr: &impl HerdrClient, pane: &str, now: Instant) -> herdr::Result<()> {
        let agent = match herdr.agent_get(pane) {
            Ok(agent) => Some(agent),
            Err(HerdrError::NotFound { .. }) => None,
            Err(err) => {
                // Keep what we have and wait a full interval before trying again.
                self.agents
                    .entry(pane.to_owned())
                    .or_insert_with(|| AgentEntry::from_agent(None, now))
                    .fetched = now;
                return Err(err);
            }
        };
        self.agents
            .insert(pane.to_owned(), AgentEntry::from_agent(agent, now));
        Ok(())
    }
}

impl AgentEntry {
    fn from_agent(agent: Option<Agent>, fetched: Instant) -> Self {
        Self {
            workspace_id: agent.as_ref().map(|a| a.workspace_id.clone()),
            info: agent.map(|a| AgentInfo {
                status: a.status,
                context: a.tokens.context,
                limit: a.tokens.limit,
            }),
            fetched,
        }
    }

    fn from_pane(pane: Pane, fetched: Instant) -> Self {
        Self {
            info: pane.agent.is_some().then_some(AgentInfo {
                status: pane.status,
                context: pane.tokens.context,
                limit: pane.tokens.limit,
            }),
            workspace_id: Some(pane.workspace_id),
            fetched,
        }
    }
}

/// The herdr pane a task's worker runs in: from its latest `session_start`, else
/// `task.json`.
fn pane_of(state: &TaskState) -> Option<&str> {
    state
        .live
        .herdr_pane_id
        .as_deref()
        .or(state.task.herdr_pane_id.as_deref())
}

/// Torn-down and abandoned tasks have no worker left; the deck hides them unless asked.
fn hidden_by_default(phase: Phase) -> bool {
    matches!(phase, Phase::TornDown | Phase::Abandoned)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use conductor_state::events_path;

    use super::*;
    use crate::deck::card::{Attention, Group};
    use crate::deck::testing::{REPO_ID, TempRepo, event, fixture, task};
    use crate::herdr::fake::FakeHerdr;
    use crate::herdr::types::{AgentStatus, PluginContext, Snapshot, Tokens};

    fn agent(pane: &str, status: AgentStatus, context: &str) -> Agent {
        Agent {
            pane_id: pane.into(),
            tab_id: "w1:t1".into(),
            workspace_id: "w1".into(),
            agent: Some("claude".into()),
            name: None,
            title: None,
            status,
            tokens: Tokens {
                context: Some(context.into()),
                limit: Some("5h 10%".into()),
            },
        }
    }

    fn fake(agents: Vec<Agent>) -> FakeHerdr {
        let context = PluginContext::from_parts(None, Some("w1:p9".into()), None, None).unwrap();
        FakeHerdr::new(
            context,
            Snapshot {
                agents,
                ..Snapshot::default()
            },
        )
    }

    fn set_agents(herdr: &FakeHerdr, agents: Vec<Agent>) {
        herdr.snapshot.lock().unwrap().agents = agents;
    }

    /// Task `a` runs in pane `p1` (from its events), task `b` in `p2` (from `task.json`).
    fn two_tasks() -> (TempRepo, Deck) {
        let repo = TempRepo::new();
        repo.add_task("a", Phase::Working, &fixture("idle.jsonl"));
        let mut b = task("b", Phase::Working);
        b.herdr_pane_id = Some("p2".into());
        repo.write_task(&b);
        let mut gone = task("gone", Phase::TornDown);
        gone.herdr_pane_id = Some("p3".into());
        repo.write_task(&gone);
        let deck = Deck::open(repo.home.clone(), REPO_ID.into()).unwrap();
        (repo, deck)
    }

    fn card<'a>(cards: &'a [CardModel], id: &str) -> &'a CardModel {
        cards.iter().find(|c| c.task.id == id).unwrap()
    }

    #[test]
    fn hides_torn_down_and_abandoned_by_default() {
        let repo = TempRepo::new();
        for (id, phase) in [
            ("live", Phase::Working),
            ("merged", Phase::Merged),
            ("down", Phase::TornDown),
            ("dropped", Phase::Abandoned),
        ] {
            repo.add_task(id, phase, &[]);
        }
        let mut deck = Deck::open(repo.home.clone(), REPO_ID.into()).unwrap();
        let ids = |deck: &Deck| -> BTreeSet<String> {
            deck.cards().into_iter().map(|c| c.task.id).collect()
        };
        assert_eq!(ids(&deck), ["live", "merged"].map(String::from).into());
        deck.show_hidden = true;
        assert_eq!(ids(&deck).len(), 4);
    }

    #[test]
    fn picks_up_new_and_removed_tasks() {
        let repo = TempRepo::new();
        let mut deck = Deck::open(repo.home.clone(), REPO_ID.into()).unwrap();
        // A directory without its task.json yet is skipped until the next change.
        fs::create_dir_all(repo.task_dir("new")).unwrap();
        deck.task_changed("new");
        assert_eq!(deck.cards().len(), 0);
        repo.add_task("new", Phase::Briefed, &[]);
        deck.task_changed("new");
        assert_eq!(deck.cards().len(), 1);

        fs::remove_dir_all(repo.task_dir("new")).unwrap();
        deck.task_changed("new");
        assert_eq!(deck.cards().len(), 0);
    }

    #[test]
    fn keeps_last_good_state_on_a_bad_read() {
        let repo = TempRepo::new();
        repo.add_task("t", Phase::Working, &fixture("working_tool.jsonl"));
        let mut deck = Deck::open(repo.home.clone(), REPO_ID.into()).unwrap();
        fs::write(repo.task_dir("t").join("task.json"), "{").unwrap();
        deck.task_changed("t");
        assert!(deck.errors().contains_key("t"));
        assert_eq!(deck.cards()[0].group, Group::Working);

        repo.add_task("t", Phase::Working, &[]);
        deck.task_changed("t");
        assert!(deck.errors().is_empty());
    }

    #[test]
    fn tick_moves_elapsed_without_reading_files() {
        let repo = TempRepo::new();
        repo.add_task("t", Phase::Working, &fixture("working_tool.jsonl"));
        let mut deck = Deck::open(repo.home.clone(), REPO_ID.into()).unwrap();
        let start = deck.cards()[0].since;
        deck.tick(start + jiff::SignedDuration::from_secs(5));
        let before = deck.cards();
        assert_eq!(before[0].elapsed(deck.now()).as_secs(), 5);

        repo.append("t", &[event("12:10:00", r#""kind":"turn_end""#)]);
        deck.tick(start + jiff::SignedDuration::from_secs(6));
        let after = deck.cards();
        assert_eq!(after, before);
        assert_eq!(after[0].elapsed(deck.now()).as_secs(), 6);
        assert!(
            fs::metadata(events_path(&repo.task_dir("t")))
                .unwrap()
                .len()
                > 0
        );
    }

    #[test]
    fn watches_panes_of_live_tasks() {
        let (_repo, deck) = two_tasks();
        assert_eq!(deck.watched_panes(), ["p1", "p2"]);
    }

    #[test]
    fn resync_seeds_agents_and_status_changes_refetch() {
        let (_repo, mut deck) = two_tasks();
        let herdr = fake(vec![agent("p1", AgentStatus::Working, "⛁ 26%")]);
        let now = Instant::now();
        deck.apply_herdr(
            &herdr,
            herdr.snapshot().map(HerdrEvent::Resync).unwrap(),
            now,
        )
        .unwrap();
        let cards = deck.cards();
        let a = card(&cards, "a");
        assert_eq!(a.agent.as_ref().unwrap().context.as_deref(), Some("⛁ 26%"));
        // Idle by its events, but herdr says it's working.
        assert_eq!(a.group, Group::Working);
        assert_eq!(card(&cards, "b").agent, None);

        set_agents(
            &herdr,
            vec![
                agent("p1", AgentStatus::Working, "⛁ 26%"),
                agent("p2", AgentStatus::Blocked, "⛁ 80%"),
            ],
        );
        let changed = HerdrEvent::AgentStatusChanged {
            pane_id: "p2".into(),
            workspace_id: "w1".into(),
            status: AgentStatus::Blocked,
        };
        deck.apply_herdr(&herdr, changed, now).unwrap();
        let cards = deck.cards();
        let b = card(&cards, "b");
        assert_eq!(b.attention, [Attention::Permission]);
        assert_eq!(b.agent.as_ref().unwrap().context.as_deref(), Some("⛁ 80%"));
        assert_eq!(cards[0].task.id, "b");
    }

    #[test]
    fn refreshes_tokens_at_most_every_ten_seconds() {
        let (_repo, mut deck) = two_tasks();
        let herdr = fake(vec![agent("p1", AgentStatus::Working, "⛁ 10%")]);
        let start = Instant::now();
        deck.refresh_stale_agents(&herdr, start).unwrap();
        let context = |deck: &Deck| {
            card(&deck.cards(), "a")
                .agent
                .as_ref()
                .and_then(|a| a.context.clone())
        };
        assert_eq!(context(&deck).as_deref(), Some("⛁ 10%"));

        set_agents(&herdr, vec![agent("p1", AgentStatus::Working, "⛁ 20%")]);
        deck.refresh_stale_agents(&herdr, start + Duration::from_secs(9))
            .unwrap();
        assert_eq!(context(&deck).as_deref(), Some("⛁ 10%"));
        deck.refresh_stale_agents(&herdr, start + AGENT_REFRESH)
            .unwrap();
        assert_eq!(context(&deck).as_deref(), Some("⛁ 20%"));
    }

    #[test]
    fn a_missing_or_closed_agent_clears() {
        let (_repo, mut deck) = two_tasks();
        let herdr = fake(vec![agent("p1", AgentStatus::Idle, "⛁ 10%")]);
        let now = Instant::now();
        deck.refresh_stale_agents(&herdr, now).unwrap();
        assert!(card(&deck.cards(), "a").agent.is_some());

        let closed = HerdrEvent::PaneClosed {
            pane_id: "p1".into(),
            workspace_id: "w1".into(),
        };
        deck.apply_herdr(&herdr, closed, now).unwrap();
        assert_eq!(card(&deck.cards(), "a").agent, None);

        deck.refresh_stale_agents(&herdr, now + AGENT_REFRESH)
            .unwrap();
        assert!(card(&deck.cards(), "a").agent.is_some());
        set_agents(&herdr, Vec::new());
        let changed = HerdrEvent::AgentStatusChanged {
            pane_id: "p1".into(),
            workspace_id: "w1".into(),
            status: AgentStatus::Idle,
        };
        deck.apply_herdr(&herdr, changed, now).unwrap();
        assert_eq!(card(&deck.cards(), "a").agent, None);
    }
}
