//! Finds the conductor repo the deck belongs to, from the herdr workspace it runs in.

use std::path::{Path, PathBuf};

use conductor_state::{Home, read_repo_record, resolve_repo};

use crate::herdr::types::{PluginContext, Snapshot};
use crate::herdr::{self, HerdrClient};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// A repo with a conductor record in `repos/<repo_id>/`; `name` is its root's directory name.
    Repo { repo_id: String, name: String },
    /// The deck's workspace has no conductor; `reason` says why.
    NoConductor { reason: String },
}

/// Works out the deck's repo: its workspace's directory from herdr, then that directory's
/// repo id from `conductor-state`, then whether conductor has a record for it.
pub fn locate(herdr: &impl HerdrClient, home: &Home) -> herdr::Result<Location> {
    let context = herdr.plugin_context()?;
    let snapshot = herdr.snapshot()?;
    Ok(match workspace_dir(&context, &snapshot) {
        Some(dir) => locate_dir(&dir, home),
        None => Location::NoConductor {
            reason: "herdr reports no directory for the deck's workspace".into(),
        },
    })
}

/// The conductor repo containing `dir`, from the main checkout or any worktree of it.
pub fn locate_dir(dir: &Path, home: &Home) -> Location {
    let no_conductor = |reason: String| Location::NoConductor { reason };
    let ident = match resolve_repo(dir) {
        Ok(ident) => ident,
        Err(err) => return no_conductor(err.to_string()),
    };
    match read_repo_record(&home.repo_dir(&ident.id)) {
        Ok(Some(_)) => Location::Repo {
            name: ident.root.file_name().map_or_else(
                || ident.id.clone(),
                |name| name.to_string_lossy().into_owned(),
            ),
            repo_id: ident.id,
        },
        Ok(None) => no_conductor(format!("{} has no conductor record", ident.root.display())),
        Err(err) => no_conductor(err.to_string()),
    }
}

/// The directory of the deck's workspace: herdr's worktree `repo_root` if it knows one,
/// else the cwd of another pane in the workspace, else the deck pane's own cwd.
pub fn workspace_dir(context: &PluginContext, snapshot: &Snapshot) -> Option<PathBuf> {
    let workspace = context.workspace_id.as_deref()?;
    let worktree = snapshot
        .workspaces
        .iter()
        .find(|ws| ws.workspace_id == workspace)
        .and_then(|ws| ws.worktree.as_ref());
    if let Some(worktree) = worktree {
        return Some(worktree.repo_root.clone());
    }
    let panes = || {
        snapshot
            .panes
            .iter()
            .filter(|pane| pane.workspace_id == workspace)
    };
    let (own, others): (Vec<_>, Vec<_>) = panes().partition(|pane| pane.pane_id == context.pane_id);
    others
        .into_iter()
        .chain(own)
        .find_map(|pane| pane.cwd.clone())
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use conductor_state::{RepoRecord, SchemaV1, repo_id_for_root, write_repo_record_atomic};

    use super::*;
    use crate::herdr::types::{AgentStatus, Pane, Tokens, Workspace, WorkspaceWorktree};

    fn context() -> PluginContext {
        PluginContext::from_parts(None, Some("w1:p9".into()), None, Some("w1".into())).unwrap()
    }

    fn pane(id: &str, workspace: &str, cwd: Option<&str>) -> Pane {
        Pane {
            pane_id: id.into(),
            tab_id: format!("{workspace}:t1"),
            workspace_id: workspace.into(),
            agent: None,
            status: AgentStatus::Unknown,
            focused: false,
            label: None,
            tokens: Tokens::default(),
            cwd: cwd.map(PathBuf::from),
        }
    }

    fn workspace(id: &str, repo_root: Option<&str>) -> Workspace {
        Workspace {
            workspace_id: id.into(),
            label: id.into(),
            focused: false,
            worktree: repo_root.map(|root| WorkspaceWorktree {
                repo_root: root.into(),
                checkout_path: root.into(),
            }),
        }
    }

    #[test]
    fn prefers_worktree_repo_root() {
        let snapshot = Snapshot {
            workspaces: vec![
                workspace("w0", Some("/other")),
                workspace("w1", Some("/src/web")),
            ],
            panes: vec![pane("w1:p1", "w1", Some("/src/web/.worktrees/x"))],
            ..Snapshot::default()
        };
        assert_eq!(
            workspace_dir(&context(), &snapshot),
            Some(PathBuf::from("/src/web"))
        );
    }

    #[test]
    fn falls_back_to_another_panes_cwd_then_the_decks() {
        let mut snapshot = Snapshot {
            workspaces: vec![workspace("w1", None)],
            panes: vec![
                pane("w2:p1", "w2", Some("/elsewhere")),
                pane("w1:p9", "w1", Some("/plugin")),
                pane("w1:p1", "w1", None),
                pane("w1:p2", "w1", Some("/src/web")),
            ],
            ..Snapshot::default()
        };
        assert_eq!(
            workspace_dir(&context(), &snapshot),
            Some(PathBuf::from("/src/web"))
        );
        snapshot.panes.retain(|pane| pane.pane_id != "w1:p2");
        assert_eq!(
            workspace_dir(&context(), &snapshot),
            Some(PathBuf::from("/plugin"))
        );
        snapshot.panes.clear();
        assert_eq!(workspace_dir(&context(), &snapshot), None);
    }

    #[test]
    fn no_workspace_no_dir() {
        let context = PluginContext::from_parts(None, Some("p".into()), None, None).unwrap();
        assert_eq!(workspace_dir(&context, &Snapshot::default()), None);
    }

    #[test]
    fn finds_conductor_record_from_a_git_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("web");
        std::fs::create_dir(&repo).unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()
            .unwrap();
        assert!(status.success());
        let home = Home::new(tmp.path().join("state"));

        assert!(matches!(
            locate_dir(&repo, &home),
            Location::NoConductor { reason } if reason.contains("no conductor record")
        ));
        assert!(matches!(
            locate_dir(tmp.path(), &home),
            Location::NoConductor { .. }
        ));

        let root = repo.canonicalize().unwrap();
        let repo_id = repo_id_for_root(&root);
        let repo_dir = home.repo_dir(&repo_id);
        std::fs::create_dir_all(&repo_dir).unwrap();
        let record = RepoRecord {
            schema: SchemaV1,
            repo_root: root,
            repo_id: repo_id.clone(),
            conductor_session: "conductor-web".into(),
            herdr_workspace_id: None,
            herdr_pane_id: None,
        };
        write_repo_record_atomic(&repo_dir, &record).unwrap();
        assert_eq!(
            locate_dir(&repo, &home),
            Location::Repo {
                repo_id,
                name: "web".into()
            }
        );
    }
}
