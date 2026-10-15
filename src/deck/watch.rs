//! Watches a repo's task directories and reports which task changed.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Something under `tasks/<id>/` changed, or the directory itself appeared or went away.
    Task(String),
    /// The watcher may have missed changes; reload every task.
    Rescan,
}

/// Stops watching when dropped.
pub struct TaskWatcher {
    _watcher: RecommendedWatcher,
}

impl TaskWatcher {
    /// Watches `repo_dir` recursively, rather than `tasks/`, so a `tasks/` created after the
    /// deck starts is still seen. `on_change` runs on the watcher's thread.
    pub fn start(
        repo_dir: &Path,
        on_change: impl Fn(Change) + Send + 'static,
    ) -> notify::Result<Self> {
        // Backends may report canonical paths (FSEvents resolves `/var` to `/private/var`),
        // so match against both spellings.
        let tasks_dirs = [
            repo_dir.join("tasks"),
            repo_dir.canonicalize()?.join("tasks"),
        ];
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                let event = match result {
                    Ok(event) if event.need_rescan() => return on_change(Change::Rescan),
                    Ok(event) => event,
                    Err(_) => return on_change(Change::Rescan),
                };
                // The deck's own reads show up as access events on some backends.
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
                }
                let mut ids: Vec<String> = event
                    .paths
                    .iter()
                    .filter_map(|path| task_id(&tasks_dirs, path))
                    .collect();
                ids.dedup();
                for id in ids {
                    on_change(Change::Task(id));
                }
            })?;
        watcher.watch(repo_dir, RecursiveMode::Recursive)?;
        Ok(Self { _watcher: watcher })
    }
}

/// The `<id>` of a path under `tasks/<id>/`, or `tasks/<id>` itself.
fn task_id(tasks_dirs: &[PathBuf], path: &Path) -> Option<String> {
    let rest = tasks_dirs
        .iter()
        .find_map(|dir| path.strip_prefix(dir).ok())?;
    match rest.components().next()? {
        Component::Normal(name) if !is_hidden(name) => Some(name.to_str()?.to_owned()),
        _ => None,
    }
}

fn is_hidden(name: &OsStr) -> bool {
    name.as_encoded_bytes().starts_with(b".")
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use conductor_state::Phase;

    use super::*;
    use crate::deck::Deck;
    use crate::deck::testing::{REPO_ID, TempRepo, event};

    #[test]
    fn maps_paths_to_task_ids() {
        let dirs = [
            PathBuf::from("/h/repos/r/tasks"),
            PathBuf::from("/p/h/repos/r/tasks"),
        ];
        let id = |path: &str| task_id(&dirs, Path::new(path));
        assert_eq!(
            id("/h/repos/r/tasks/fix-07/events.jsonl"),
            Some("fix-07".into())
        );
        assert_eq!(
            id("/p/h/repos/r/tasks/fix-07/decisions/.db.json.1.0.tmp"),
            Some("fix-07".into())
        );
        assert_eq!(id("/h/repos/r/tasks/new-01"), Some("new-01".into()));
        assert_eq!(id("/h/repos/r/tasks"), None);
        assert_eq!(id("/h/repos/r/tasks/.DS_Store"), None);
        assert_eq!(id("/h/repos/r/repo.json"), None);
    }

    /// Waits for `want`, skipping other changes, or fails after a few seconds.
    fn expect_change(rx: &mpsc::Receiver<Change>, want: &Change) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(change) if &change == want => return,
                Ok(_) => {}
                Err(err) => panic!("no {want:?}: {err}"),
            }
        }
    }

    #[test]
    fn a_task_created_while_running_appears() {
        // No `tasks/` yet, as for a repo whose conductor hasn't made a task.
        let repo = TempRepo::new();
        let mut deck = Deck::open(repo.home.clone(), REPO_ID.into()).unwrap();
        assert_eq!(deck.cards().len(), 0);

        let (tx, rx) = mpsc::channel();
        let _watcher = TaskWatcher::start(&repo.repo_dir(), move |change| {
            let _ = tx.send(change);
        })
        .unwrap();

        repo.add_task("new-01", Phase::Working, &[]);
        expect_change(&rx, &Change::Task("new-01".into()));
        deck.task_changed("new-01");
        let titles: Vec<_> = deck.cards().into_iter().map(|c| c.task.title).collect();
        assert_eq!(titles, ["Task new-01"]);

        // And its events are picked up as they're appended.
        repo.append(
            "new-01",
            &[event("12:00:00", r#""kind":"prompt","text":"go""#)],
        );
        expect_change(&rx, &Change::Task("new-01".into()));
        deck.task_changed("new-01");
        assert!(matches!(
            deck.cards()[0].live.activity,
            conductor_state::Activity::Working { .. }
        ));
    }
}
