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

/// The sort groups, top of the deck first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    NeedsAttention,
    AwaitingMerge,
    Working,
    Other,
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
    /// Empty unless `group` is [`Group::NeedsAttention`].
    pub attention: Vec<Attention>,
    pub group: Group,
    /// When the card entered the state that puts it in `group`.
    pub since: Timestamp,
}

impl CardModel {
    pub fn new(state: &TaskState, agent: Option<&AgentInfo>) -> Self {
        let attention = attention(state, agent);
        let agent_working = agent.is_some_and(|a| a.status == AgentStatus::Working);
        let group = if !attention.is_empty() {
            Group::NeedsAttention
        } else if state.task.phase == Phase::AwaitingMerge {
            Group::AwaitingMerge
        } else if matches!(state.live.activity, Activity::Working { .. }) || agent_working {
            Group::Working
        } else {
            Group::Other
        };
        let since = match group {
            Group::NeedsAttention => attention
                .iter()
                .filter_map(|reason| reason_since(state, *reason))
                .min(),
            Group::AwaitingMerge => state.phase_changed(),
            Group::Working | Group::Other => state.live.since,
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
            group,
            since,
        }
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

/// Needs attention first, then awaiting merge, working, and the rest; the longest in its
/// state first within a group.
pub fn sort(cards: &mut [CardModel]) {
    cards.sort_by(|a, b| (a.group, a.since, &a.task.id).cmp(&(b.group, b.since, &b.task.id)));
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
    fn fixtures_land_in_their_groups() {
        let cases = [
            (
                "needs_permission.jsonl",
                Phase::Working,
                Group::NeedsAttention,
            ),
            (
                "decisions.jsonl",
                Phase::NeedsDecision,
                Group::NeedsAttention,
            ),
            (
                "pr_lifecycle.jsonl",
                Phase::AwaitingMerge,
                Group::AwaitingMerge,
            ),
            ("working_tool.jsonl", Phase::Working, Group::Working),
            (
                "working_between_tools.jsonl",
                Phase::Working,
                Group::Working,
            ),
            ("compact_mid_turn.jsonl", Phase::Working, Group::Working),
            ("permission_granted.jsonl", Phase::Working, Group::Working),
            ("unknown_kind.jsonl", Phase::Working, Group::Working),
            ("idle.jsonl", Phase::Review, Group::Other),
            ("idle_error.jsonl", Phase::Working, Group::Other),
            ("exited.jsonl", Phase::Working, Group::Other),
            ("resumed.jsonl", Phase::Working, Group::Other),
            ("pr_closed.jsonl", Phase::Working, Group::Working),
        ];
        for (name, phase, group) in cases {
            assert_eq!(card(phase, &fixture(name), None).group, group, "{name}");
        }
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
        assert_eq!(approved.group, Group::AwaitingMerge);
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
        assert_eq!(working.group, Group::Working);
        let idle = card(Phase::Working, &lines, Some(&agent(AgentStatus::Idle)));
        assert_eq!(idle.group, Group::Other);
    }

    #[test]
    fn no_events_falls_back_to_updated_at() {
        let card = card(Phase::Briefed, &[], None);
        assert_eq!(card.group, Group::Other);
        assert_eq!(card.since, ts("11:00:00"));
        assert_eq!(card.elapsed(ts("11:01:30")), SignedDuration::from_secs(90));
    }

    #[test]
    fn sorts_by_group_then_oldest_first() {
        let make = |id: &str, phase, lines: &[String]| {
            let mut card = card(phase, lines, None);
            card.task.id = id.into();
            card
        };
        let start = r#""kind":"session_start","source":"startup""#;
        let prompt = r#""kind":"prompt","text":"go""#;
        let permission = r#""kind":"attention","type":"permission_prompt","message":"Bash""#;
        let mut cards = vec![
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
                "working-old",
                "working-new",
                "idle"
            ]
        );
    }
}
