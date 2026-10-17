//! What one card shows, and where it sorts.

use conductor_state::{
    Activity, CheckState, Decision, LiveStatus, Phase, PrState, ReviewDecision, StatusVerb, Task,
};
use jiff::{SignedDuration, Timestamp};

use super::task::TaskState;
use crate::herdr::types::AgentStatus;

/// herdr's view of a task's agent: from `agent get`, a snapshot, or a pane update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInfo {
    pub status: AgentStatus,
    /// herdr's context readout, e.g. `⛁ 26% (263k)`.
    pub context: Option<String>,
    /// herdr's usage-limit readout, e.g. `5h 68%`.
    pub limit: Option<String>,
}

/// The deck's sections, top first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    NeedsYou,
    Review,
    InProgress,
    Todo,
    Done,
}

/// Where a task stands, as the card's badge shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// A permission prompt, an open decision, a `blocked` report or requested changes.
    NeedsYou,
    /// The worker or the PR's checks failed, or the task's phase is `failed`.
    Failed,
    /// The worker is running, mid-turn or between turns.
    InProgress,
    /// The conductor is reviewing the worker's draft PR or report.
    Review,
    /// The PR is ready; waiting for a human to merge it.
    ReadyToMerge,
    /// No worker yet.
    Pending,
    /// Merged, torn down or abandoned.
    Done,
}

impl State {
    pub fn section(self) -> Section {
        match self {
            Self::NeedsYou | Self::Failed => Section::NeedsYou,
            Self::InProgress => Section::InProgress,
            Self::Review | Self::ReadyToMerge => Section::Review,
            Self::Pending => Section::Todo,
            Self::Done => Section::Done,
        }
    }
}

/// Why a card needs attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attention {
    /// A permission prompt, from the hooks or from herdr.
    Permission,
    /// An open decision.
    Decision,
    /// The worker reported `blocked`.
    Blocked,
    /// The worker reported `failed`, or the task's phase is `failed`.
    Failed,
    /// The open PR's checks are failing.
    ChecksFailing,
    /// The open PR has changes requested.
    ChangesRequested,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CardModel {
    pub task: Task,
    pub live: LiveStatus,
    pub agent: Option<AgentInfo>,
    /// Oldest first.
    pub open_decisions: Vec<Decision>,
    /// Empty unless `state` is [`State::NeedsYou`] or [`State::Failed`].
    pub attention: Vec<Attention>,
    pub state: State,
    /// When the card entered the state that puts it in its section.
    pub since: Timestamp,
}

impl CardModel {
    pub fn new(state: &TaskState, agent: Option<&AgentInfo>) -> Self {
        let attention = attention(state, agent);
        let working = working(&state.live, agent);
        let card_state = card_state(state.task.phase, &attention, working);
        let since = match card_state.section() {
            Section::NeedsYou => attention
                .iter()
                .filter_map(|reason| reason_since(state, *reason))
                .min(),
            Section::Review | Section::Done => state.phase_changed(),
            Section::InProgress | Section::Todo => state.live.since,
        }
        .unwrap_or(state.task.updated_at);
        let mut open_decisions = state.open_decisions.clone();
        open_decisions.sort_by_cached_key(|d| (state.decision_opened(&d.key), d.key.clone()));
        Self {
            task: state.task.clone(),
            live: state.live.clone(),
            agent: agent.cloned(),
            open_decisions,
            attention,
            state: card_state,
            since,
        }
    }

    pub fn section(&self) -> Section {
        self.state.section()
    }

    /// Whether the worker is mid-turn, by its events or by herdr.
    pub fn working(&self) -> bool {
        working(&self.live, self.agent.as_ref())
    }

    /// Time in the current state as of `now`.
    pub fn elapsed(&self, now: Timestamp) -> SignedDuration {
        now.duration_since(self.since)
    }

    /// The herdr pane the worker runs in: from its latest `session_start`, else `task.json`.
    pub fn pane(&self) -> Option<&str> {
        (self.live.herdr_pane_id.as_deref()).or(self.task.herdr_pane_id.as_deref())
    }

    /// The task's PR: from `task.json`, else the worker's latest report.
    pub fn pr_url(&self) -> Option<&str> {
        self.task.pr_url.as_deref().or(self.live.pr_url.as_deref())
    }
}

/// By section, then the longest in its state first.
pub fn sort(cards: &mut [CardModel]) {
    cards.sort_by(|a, b| {
        (a.section(), a.since, &a.task.id).cmp(&(b.section(), b.since, &b.task.id))
    });
}

fn working(live: &LiveStatus, agent: Option<&AgentInfo>) -> bool {
    matches!(live.activity, Activity::Working { .. })
        || agent.is_some_and(|a| a.status == AgentStatus::Working)
}

/// Attention first, then the phase; a running worker counts as in progress until review.
fn card_state(phase: Phase, attention: &[Attention], working: bool) -> State {
    let needs_you = attention.iter().any(|reason| {
        matches!(
            reason,
            Attention::Permission
                | Attention::Decision
                | Attention::Blocked
                | Attention::ChangesRequested
        )
    });
    if needs_you {
        return State::NeedsYou;
    }
    if !attention.is_empty() {
        return State::Failed;
    }
    match phase {
        Phase::Merged | Phase::TornDown | Phase::Abandoned => State::Done,
        Phase::AwaitingMerge => State::ReadyToMerge,
        Phase::Failed => State::Failed,
        _ if working => State::InProgress,
        Phase::Review => State::Review,
        Phase::Working | Phase::NeedsDecision => State::InProgress,
        Phase::Intake | Phase::Briefed | Phase::Spawning => State::Pending,
    }
}

fn attention(state: &TaskState, agent: Option<&AgentInfo>) -> Vec<Attention> {
    let live = &state.live;
    let mut reasons = Vec::new();
    if matches!(live.activity, Activity::NeedsPermission { .. })
        || agent.is_some_and(|a| a.status == AgentStatus::Blocked)
    {
        reasons.push(Attention::Permission);
    }
    if !live.open_decisions.is_empty() {
        reasons.push(Attention::Decision);
    }
    match live.status.as_ref().map(|s| s.verb) {
        Some(StatusVerb::Blocked) => reasons.push(Attention::Blocked),
        Some(StatusVerb::Failed) => reasons.push(Attention::Failed),
        _ if state.task.phase == Phase::Failed => reasons.push(Attention::Failed),
        _ => {}
    }
    if let Some(pr) = live
        .pr
        .filter(|pr| matches!(pr.state, PrState::Open | PrState::Draft))
    {
        if pr.checks == Some(CheckState::Fail) {
            reasons.push(Attention::ChecksFailing);
        }
        if pr.review == Some(ReviewDecision::ChangesRequested) {
            reasons.push(Attention::ChangesRequested);
        }
    }
    reasons
}

fn reason_since(state: &TaskState, reason: Attention) -> Option<Timestamp> {
    let live = &state.live;
    match reason {
        // A prompt herdr sees but the hooks don't has no time of its own.
        Attention::Permission => live.since,
        Attention::Decision => live
            .open_decisions
            .iter()
            .filter_map(|key| state.decision_opened(key))
            .min(),
        Attention::Blocked => live.status.as_ref().map(|s| s.ts),
        Attention::Failed => live
            .status
            .as_ref()
            .filter(|s| s.verb == StatusVerb::Failed)
            .map(|s| s.ts)
            .or_else(|| state.phase_changed()),
        Attention::ChecksFailing | Attention::ChangesRequested => live.pr.map(|pr| pr.ts),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::testing::{TempRepo, event, fixture, ts};

    fn card(phase: Phase, lines: &[String], agent: Option<&AgentInfo>) -> CardModel {
        let repo = TempRepo::new();
        repo.add_task("t", phase, lines);
        let state = TaskState::load(&repo.task_dir("t")).unwrap().unwrap();
        CardModel::new(&state, agent)
    }

    fn agent(status: AgentStatus) -> AgentInfo {
        AgentInfo {
            status,
            context: None,
            limit: None,
        }
    }

    #[test]
    fn fixtures_land_in_their_states() {
        // The state, and whether the worker is mid-turn.
        let cases = [
            (
                "needs_permission.jsonl",
                Phase::Working,
                State::NeedsYou,
                false,
            ),
            (
                "decisions.jsonl",
                Phase::NeedsDecision,
                State::NeedsYou,
                false,
            ),
            (
                "pr_lifecycle.jsonl",
                Phase::AwaitingMerge,
                State::ReadyToMerge,
                true,
            ),
            (
                "working_tool.jsonl",
                Phase::Working,
                State::InProgress,
                true,
            ),
            (
                "working_between_tools.jsonl",
                Phase::Working,
                State::InProgress,
                true,
            ),
            (
                "compact_mid_turn.jsonl",
                Phase::Working,
                State::InProgress,
                true,
            ),
            (
                "permission_granted.jsonl",
                Phase::Working,
                State::InProgress,
                true,
            ),
            (
                "unknown_kind.jsonl",
                Phase::Working,
                State::InProgress,
                true,
            ),
            ("idle.jsonl", Phase::Review, State::Review, false),
            ("idle_error.jsonl", Phase::Working, State::InProgress, false),
            ("exited.jsonl", Phase::Working, State::InProgress, false),
            ("resumed.jsonl", Phase::Working, State::InProgress, false),
            ("pr_closed.jsonl", Phase::Working, State::InProgress, true),
            ("idle.jsonl", Phase::Failed, State::Failed, false),
            ("exited.jsonl", Phase::Merged, State::Done, false),
        ];
        for (name, phase, state, working) in cases {
            let card = card(phase, &fixture(name), None);
            assert_eq!((card.state, card.working()), (state, working), "{name}");
        }
    }

    #[test]
    fn phases_without_events() {
        let cases = [
            (Phase::Intake, State::Pending),
            (Phase::Briefed, State::Pending),
            (Phase::Spawning, State::Pending),
            (Phase::Working, State::InProgress),
            (Phase::Review, State::Review),
            (Phase::AwaitingMerge, State::ReadyToMerge),
            (Phase::Failed, State::Failed),
            (Phase::Merged, State::Done),
            (Phase::TornDown, State::Done),
            (Phase::Abandoned, State::Done),
        ];
        for (phase, state) in cases {
            assert_eq!(card(phase, &[], None).state, state, "{phase:?}");
        }
    }

    #[test]
    fn a_running_worker_in_review_is_in_progress() {
        let card = card(Phase::Review, &fixture("working_tool.jsonl"), None);
        assert_eq!(card.state, State::InProgress);
    }

    #[test]
    fn needs_permission_since_the_prompt() {
        let card = card(Phase::Working, &fixture("needs_permission.jsonl"), None);
        assert_eq!(card.attention, [Attention::Permission]);
        assert_eq!(card.since, ts("12:00:11"));
    }

    #[test]
    fn open_decision_and_blocked_report_since_the_earliest() {
        let card = card(Phase::NeedsDecision, &fixture("decisions.jsonl"), None);
        assert_eq!(card.attention, [Attention::Decision, Attention::Blocked]);
        // auth-flow opened at 12:02:01; db-engine (12:02:00) was answered.
        assert_eq!(card.since, ts("12:02:01"));
    }

    #[test]
    fn open_decisions_oldest_first() {
        let repo = TempRepo::new();
        let open = |time, key| event(time, &format!(r#""kind":"decision_open","key":"{key}""#));
        let lines = [open("12:00:00", "zeta"), open("12:01:00", "alpha")];
        repo.add_task("t", Phase::NeedsDecision, &lines);
        repo.add_decision("t", "alpha");
        repo.add_decision("t", "zeta");
        let state = TaskState::load(&repo.task_dir("t")).unwrap().unwrap();
        let card = CardModel::new(&state, None);
        let keys: Vec<_> = card.open_decisions.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(keys, ["zeta", "alpha"]);
    }

    #[test]
    fn pr_lifecycle_moves_between_attention_and_awaiting_merge() {
        let lines = fixture("pr_lifecycle.jsonl");
        // Up to `checks: fail`.
        let failing = card(Phase::AwaitingMerge, &lines[..6], None);
        assert_eq!(failing.attention, [Attention::ChecksFailing]);
        assert_eq!(failing.since, ts("12:08:00"));
        // Up to `changes_requested`.
        let changes = card(Phase::AwaitingMerge, &lines[..7], None);
        assert_eq!(changes.attention, [Attention::ChangesRequested]);
        assert_eq!(changes.since, ts("12:20:00"));
        // Approved, then merged.
        let approved = card(Phase::AwaitingMerge, &lines[..8], None);
        assert_eq!(approved.state, State::ReadyToMerge);
        assert_eq!(approved.since, ts("12:08:00"));
    }

    #[test]
    fn a_closed_prs_failing_checks_need_nothing() {
        let lines = [event(
            "12:00:00",
            r#""kind":"pr","state":"closed","checks":"fail""#,
        )];
        assert_eq!(card(Phase::Working, &lines, None).attention, []);
    }

    #[test]
    fn failed_phase_needs_attention() {
        let card = card(Phase::Failed, &fixture("idle.jsonl"), None);
        // The last report was `done`, so the phase alone flags it.
        assert_eq!(card.attention, [Attention::Failed]);
        assert_eq!(card.since, ts("11:00:00"));
    }

    #[test]
    fn herdr_status_counts() {
        let lines = fixture("idle.jsonl");
        let blocked = card(Phase::Working, &lines, Some(&agent(AgentStatus::Blocked)));
        assert_eq!(blocked.attention, [Attention::Permission]);
        let working = card(Phase::Working, &lines, Some(&agent(AgentStatus::Working)));
        assert!(working.working());
        let idle = card(Phase::Working, &lines, Some(&agent(AgentStatus::Idle)));
        assert!(!idle.working());
    }

    #[test]
    fn no_events_falls_back_to_updated_at() {
        let card = card(Phase::Briefed, &[], None);
        assert_eq!(card.state, State::Pending);
        assert_eq!(card.since, ts("11:00:00"));
        assert_eq!(card.elapsed(ts("11:01:30")), SignedDuration::from_secs(90));
    }

    #[test]
    fn sorts_by_section_then_oldest_first() {
        let make = |id: &str, phase, lines: &[String]| {
            let mut card = card(phase, lines, None);
            card.task.id = id.into();
            card
        };
        let start = r#""kind":"session_start","source":"startup""#;
        let prompt = r#""kind":"prompt","text":"go""#;
        let permission = r#""kind":"attention","type":"permission_prompt","message":"Bash""#;
        let mut cards = vec![
            make("merged", Phase::Merged, &[]),
            make("brief", Phase::Briefed, &[]),
            make("idle", Phase::Working, &[event("12:00:00", start)]),
            make("working-new", Phase::Working, &[event("12:05:00", prompt)]),
            make("merge", Phase::AwaitingMerge, &[]),
            make("working-old", Phase::Working, &[event("12:01:00", prompt)]),
            make("perm-new", Phase::Working, &[event("12:09:00", permission)]),
            make("perm-old", Phase::Working, &[event("12:03:00", permission)]),
        ];
        sort(&mut cards);
        let ids: Vec<_> = cards.iter().map(|c| c.task.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "perm-old",
                "perm-new",
                "merge",
                "idle",
                "working-old",
                "working-new",
                "brief",
                "merged"
            ]
        );
    }
}
