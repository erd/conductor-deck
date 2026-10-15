//! A throwaway conductor home for model tests, filled from the fixture event logs that
//! `conductor-state` ships (copied into `tests/fixtures/`).

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use conductor_state::{
    Decision, DecisionOption, Home, Phase, SchemaV1, Task, TaskKind, events_path,
    write_decision_atomic, write_task_atomic,
};
use jiff::Timestamp;
use tempfile::TempDir;

pub const REPO_ID: &str = "web-3f9a1c";

/// A fixture's lines, without their newlines.
pub fn fixture(name: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{}: {err}", path.display()))
        .lines()
        .map(str::to_owned)
        .collect()
}

/// A timestamp on the fixtures' day.
pub fn ts(time: &str) -> Timestamp {
    format!("2026-10-14T{time}Z").parse().unwrap()
}

/// One event line for task `t` at `time`; `payload` holds `kind` and its fields.
pub fn event(time: &str, payload: &str) -> String {
    format!(r#"{{"ts":"2026-10-14T{time}Z","task":"t",{payload}}}"#)
}

pub fn task(id: &str, phase: Phase) -> Task {
    let ts = ts("11:00:00");
    Task {
        schema: SchemaV1,
        id: id.into(),
        title: format!("Task {id}"),
        kind: TaskKind::Ship,
        phase,
        branch: None,
        worktree_path: None,
        session_name: None,
        claude_session_id: None,
        herdr_workspace_id: None,
        herdr_pane_id: None,
        pr_url: None,
        created_at: ts,
        updated_at: ts,
    }
}

/// An open decision titled `Pick <key>`, offering `options` as `(value, label, hint)`.
pub fn decision(key: &str, options: &[(&str, &str, Option<&str>)]) -> Decision {
    Decision {
        schema: SchemaV1,
        key: key.into(),
        title: format!("Pick {key}"),
        about: String::new(),
        options: options
            .iter()
            .map(|&(value, label, hint)| DecisionOption {
                value: value.into(),
                label: label.into(),
                hint: hint.map(Into::into),
            })
            .collect(),
        recommend: None,
        allow_freeform: false,
        answer: None,
    }
}

pub struct TempRepo {
    _tmp: TempDir,
    pub home: Home,
}

impl TempRepo {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::new(tmp.path());
        fs::create_dir_all(home.repo_dir(REPO_ID)).unwrap();
        Self { _tmp: tmp, home }
    }

    pub fn repo_dir(&self) -> PathBuf {
        self.home.repo_dir(REPO_ID)
    }

    pub fn task_dir(&self, id: &str) -> PathBuf {
        self.home.task_dir(REPO_ID, id)
    }

    pub fn add_task(&self, id: &str, phase: Phase, events: &[String]) {
        self.write_task(&task(id, phase));
        self.append(id, events);
    }

    pub fn write_task(&self, task: &Task) {
        let dir = self.task_dir(&task.id);
        fs::create_dir_all(&dir).unwrap();
        write_task_atomic(&dir, task).unwrap();
    }

    pub fn add_decision(&self, id: &str, key: &str) {
        let decision = Decision {
            schema: SchemaV1,
            key: key.into(),
            title: format!("Pick {key}"),
            about: String::new(),
            options: Vec::new(),
            recommend: None,
            allow_freeform: true,
            answer: None,
        };
        write_decision_atomic(&self.task_dir(id), &decision).unwrap();
    }

    pub fn append(&self, id: &str, lines: &[String]) {
        for line in lines {
            self.append_raw(id, &format!("{line}\n"));
        }
    }

    /// Appends bytes as-is, to leave a line half written.
    pub fn append_raw(&self, id: &str, text: &str) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(events_path(&self.task_dir(id)))
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }
}
