//! One task as read from its directory, kept current by tailing `events.jsonl`.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::Path;

use conductor_state::{
    Decision, Error, Event, EventPayload, LiveStatus, Result, Task, events_path, read_decisions,
    read_events, read_task,
};
use jiff::Timestamp;

#[derive(Debug, Clone)]
pub struct TaskState {
    pub task: Task,
    pub live: LiveStatus,
    /// The decisions in `live.open_decisions` that have a file, sorted by key.
    pub open_decisions: Vec<Decision>,
    /// When each still-open decision was opened. `LiveStatus` keeps only the keys.
    decision_opened: BTreeMap<String, Timestamp>,
    /// When the latest `phase` event moved the task.
    phase_changed: Option<Timestamp>,
    /// Byte offset in `events.jsonl` up to which events are folded in.
    cursor: u64,
}

impl TaskState {
    /// Reads a task from scratch. `None` until its `task.json` exists.
    pub fn load(task_dir: &Path) -> Result<Option<Self>> {
        let Some(task) = read_task_if_exists(task_dir)? else {
            return Ok(None);
        };
        let mut state = Self {
            task,
            live: LiveStatus::default(),
            open_decisions: Vec::new(),
            decision_opened: BTreeMap::new(),
            phase_changed: None,
            cursor: 0,
        };
        state.read_rest(task_dir)?;
        Ok(Some(state))
    }

    /// Re-reads `task.json` and the decisions, and folds in only the events appended since
    /// the last read. Returns `false` if `task.json` is gone.
    pub fn refresh(&mut self, task_dir: &Path) -> Result<bool> {
        let Some(task) = read_task_if_exists(task_dir)? else {
            return Ok(false);
        };
        self.task = task;
        self.read_rest(task_dir)?;
        Ok(true)
    }

    pub fn decision_opened(&self, key: &str) -> Option<Timestamp> {
        self.decision_opened.get(key).copied()
    }

    pub fn phase_changed(&self) -> Option<Timestamp> {
        self.phase_changed
    }

    #[cfg(test)]
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    fn read_rest(&mut self, task_dir: &Path) -> Result<()> {
        self.read_new_events(task_dir)?;
        self.open_decisions = read_decisions(task_dir)?;
        self.open_decisions
            .retain(|decision| self.live.open_decisions.contains(&decision.key));
        Ok(())
    }

    fn read_new_events(&mut self, task_dir: &Path) -> Result<()> {
        let path = events_path(task_dir);
        let batch = match read_events(&path, self.cursor) {
            // The log only grows, so a shorter one was replaced: fold it again from the start.
            Err(Error::OffsetPastEnd { .. }) => {
                self.live = LiveStatus::default();
                self.decision_opened.clear();
                self.phase_changed = None;
                read_events(&path, 0)?
            }
            batch => batch?,
        };
        for event in &batch.events {
            self.apply(event);
        }
        self.cursor = batch.offset;
        Ok(())
    }

    fn apply(&mut self, event: &Event) {
        self.live.apply(event);
        match &event.payload {
            EventPayload::DecisionOpen { key } => {
                self.decision_opened.entry(key.clone()).or_insert(event.ts);
            }
            EventPayload::DecisionAnswered { key } => {
                self.decision_opened.remove(key);
            }
            EventPayload::Phase { .. } => self.phase_changed = Some(event.ts),
            _ => {}
        }
    }
}

fn read_task_if_exists(task_dir: &Path) -> Result<Option<Task>> {
    match read_task(task_dir) {
        Ok(task) => Ok(Some(task)),
        Err(Error::Io { source, .. }) if source.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use conductor_state::{Activity, Phase, events_path};

    use super::*;
    use crate::deck::testing::{TempRepo, fixture, ts};

    #[test]
    fn waits_for_task_json() {
        let repo = TempRepo::new();
        let dir = repo.task_dir("new-01");
        fs::create_dir_all(&dir).unwrap();
        assert!(TaskState::load(&dir).unwrap().is_none());
    }

    #[test]
    fn folds_events_and_tracks_decision_and_phase_times() {
        let repo = TempRepo::new();
        repo.add_task(
            "fix-login-07",
            Phase::NeedsDecision,
            &fixture("decisions.jsonl"),
        );
        repo.add_decision("fix-login-07", "auth-flow");
        repo.add_decision("fix-login-07", "db-engine");
        let state = TaskState::load(&repo.task_dir("fix-login-07"))
            .unwrap()
            .unwrap();
        assert_eq!(state.live.activity, Activity::Idle { error: None });
        // db-engine was answered, so only auth-flow is open.
        let keys: Vec<_> = state.open_decisions.iter().map(|d| &d.key).collect();
        assert_eq!(keys, ["auth-flow"]);
        assert_eq!(state.decision_opened("auth-flow"), Some(ts("12:02:01")));
        assert_eq!(state.decision_opened("db-engine"), None);
        assert_eq!(state.phase_changed(), None);

        repo.add_task("pr", Phase::AwaitingMerge, &fixture("pr_lifecycle.jsonl"));
        let state = TaskState::load(&repo.task_dir("pr")).unwrap().unwrap();
        assert_eq!(state.phase_changed(), Some(ts("12:08:00")));
    }

    #[test]
    fn tails_only_new_lines() {
        let repo = TempRepo::new();
        let lines = fixture("working_tool.jsonl");
        let (head, tail) = lines.split_at(3);
        repo.add_task("t", Phase::Working, head);
        let dir = repo.task_dir("t");
        let mut state = TaskState::load(&dir).unwrap().unwrap();
        let first = state.cursor();
        assert_eq!(first, fs::metadata(events_path(&dir)).unwrap().len());

        // A line still being written isn't read until it ends.
        repo.append("t", &tail[..1]);
        repo.append_raw("t", "{\"ts\":");
        assert!(state.refresh(&dir).unwrap());
        assert!(state.cursor() > first);
        assert!(matches!(
            state.live.activity,
            Activity::Working { tool: None }
        ));

        repo.append_raw("t", &tail[1][6..]);
        repo.append_raw("t", "\n");
        assert!(state.refresh(&dir).unwrap());
        let Activity::Working { tool: Some(tool) } = &state.live.activity else {
            panic!("{:?}", state.live.activity);
        };
        assert_eq!(tool.name, "Edit");
        assert_eq!(state.live.since, Some(ts("12:00:01")));
    }

    #[test]
    fn refolds_a_replaced_log() {
        let repo = TempRepo::new();
        repo.add_task("t", Phase::Working, &fixture("needs_permission.jsonl"));
        let dir = repo.task_dir("t");
        let mut state = TaskState::load(&dir).unwrap().unwrap();
        assert!(matches!(
            state.live.activity,
            Activity::NeedsPermission { .. }
        ));
        fs::write(events_path(&dir), "").unwrap();
        repo.append("t", &fixture("exited.jsonl")[..1]);
        assert!(state.refresh(&dir).unwrap());
        assert_eq!(state.live.activity, Activity::Idle { error: None });
        assert_eq!(state.live.since, Some(ts("12:00:00")));
    }

    #[test]
    fn refresh_reports_a_removed_task_json() {
        let repo = TempRepo::new();
        repo.add_task("t", Phase::Working, &[]);
        let dir = repo.task_dir("t");
        let mut state = TaskState::load(&dir).unwrap().unwrap();
        fs::remove_file(dir.join("task.json")).unwrap();
        assert!(!state.refresh(&dir).unwrap());
    }
}
