//! What the deck's keys do to a card, the conductor, or the browser. Each action returns the
//! toast to show, if any.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use conductor_state::read_repo_record;

use crate::deck::CardModel;
use crate::herdr::types::{AgentStatus, PluginContext};
use crate::herdr::{HerdrClient, HerdrError};

const BLOCKED: &str = "worker is waiting on a prompt; press Enter to focus it";

fn gone(task_id: &str) -> String {
    format!("{task_id}: worker pane is gone; run `conductor resume`")
}

/// Jumps to the card's worker.
pub fn focus_worker(herdr: &impl HerdrClient, card: &CardModel) -> Option<String> {
    let Some(pane) = card.pane() else {
        return Some(gone(&card.task.id));
    };
    match herdr.agent_focus(pane) {
        Ok(()) => None,
        Err(HerdrError::NotFound { .. }) => Some(gone(&card.task.id)),
        Err(err) => Some(format!("focus failed: {err}")),
    }
}

/// The pane to message, checked before the input opens so a message herdr would refuse isn't
/// typed for nothing; else the toast saying why not.
pub fn start_message(card: &CardModel) -> Result<String, String> {
    let pane = card.pane().ok_or_else(|| gone(&card.task.id))?;
    if card
        .agent
        .as_ref()
        .is_some_and(|agent| agent.status == AgentStatus::Blocked)
    {
        return Err(BLOCKED.to_owned());
    }
    Ok(pane.to_owned())
}

/// Sends `text` to the worker in `pane` as a new prompt. Blank text sends nothing.
pub fn send_message(
    herdr: &impl HerdrClient,
    task_id: &str,
    pane: &str,
    text: &str,
) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(match herdr.agent_prompt(pane, text) {
        Ok(()) => format!("sent to {task_id}"),
        Err(HerdrError::AgentBlocked(_)) => BLOCKED.to_owned(),
        Err(HerdrError::NotFound { .. }) => gone(task_id),
        Err(err) => format!("message failed: {err}"),
    })
}

/// Opens the card's PR with `launch`. Only web URLs are opened: the URL comes from what a
/// worker reported, and anything else could reach `open` as a flag or a local path.
pub fn open_pr(card: &CardModel, launch: impl FnOnce(&str) -> io::Result<()>) -> Option<String> {
    let Some(url) = card.pr_url() else {
        return Some(format!("{}: no PR yet", card.task.id));
    };
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Some(format!("not a web URL: {url}"));
    }
    launch(url)
        .err()
        .map(|err| format!("couldn't open the PR: {err}"))
}

/// Opens `url` in the default browser. Plugin panes start with a minimal `PATH`, so the
/// opener is found by absolute path.
pub fn launch_browser(url: &str) -> io::Result<()> {
    let opener = browser_opener()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no `open` or `xdg-open` found"))?;
    // Its output would draw over the deck.
    let mut child = Command::new(opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::Builder::new()
        .name("browser".into())
        .spawn(move || child.wait())?;
    Ok(())
}

fn browser_opener() -> Option<&'static Path> {
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &["/usr/bin/open"]
    } else {
        &[
            "/usr/bin/xdg-open",
            "/usr/local/bin/xdg-open",
            "/run/current-system/sw/bin/xdg-open",
        ]
    };
    candidates.iter().map(Path::new).find(|path| path.exists())
}

/// Jumps to the conductor: its pane from `repo.json`, read fresh since it changes when the
/// conductor restarts. Without one, or once it's gone, the deck's own tab, which the deck
/// shares with the conductor.
pub fn focus_conductor(
    herdr: &impl HerdrClient,
    repo_dir: &Path,
    context: &PluginContext,
) -> Option<String> {
    let pane = match read_repo_record(repo_dir) {
        Ok(record) => record.and_then(|record| record.herdr_pane_id),
        Err(err) => return Some(format!("repo.json: {err}")),
    };
    if let Some(pane) = pane {
        match herdr.agent_focus(&pane) {
            Ok(()) => return None,
            Err(HerdrError::NotFound { .. }) => {}
            Err(err) => return Some(format!("focus failed: {err}")),
        }
    }
    let Some(tab) = &context.tab_id else {
        return Some("no conductor pane recorded".to_owned());
    };
    herdr
        .tab_focus(tab)
        .err()
        .map(|err| format!("focus failed: {err}"))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use conductor_state::{Phase, RepoRecord, SchemaV1, write_repo_record_atomic};

    use super::*;
    use crate::deck::testing::{REPO_ID, TempRepo, task};
    use crate::deck::{AgentInfo, Deck};
    use crate::herdr::fake::{Action, FakeHerdr};
    use crate::herdr::types::Snapshot;

    fn fake(tab: Option<&str>) -> FakeHerdr {
        let context =
            PluginContext::from_parts(None, Some("w1:p9".into()), tab.map(Into::into), None)
                .unwrap();
        FakeHerdr::new(context, Snapshot::default())
    }

    /// A card for task `t`, in `pane` if given, with `pr_url` if given.
    fn card(pane: Option<&str>, pr_url: Option<&str>) -> CardModel {
        let repo = TempRepo::new();
        let mut task = task("t", Phase::Working);
        task.herdr_pane_id = pane.map(Into::into);
        task.pr_url = pr_url.map(Into::into);
        repo.write_task(&task);
        Deck::open(repo.home.clone(), REPO_ID.into())
            .unwrap()
            .cards()
            .remove(0)
    }

    fn with_status(mut card: CardModel, status: AgentStatus) -> CardModel {
        card.agent = Some(AgentInfo {
            status,
            context: None,
            limit: None,
        });
        card
    }

    const GONE: &str = "t: worker pane is gone; run `conductor resume`";

    #[test]
    fn focus_jumps_to_the_workers_pane() {
        let herdr = fake(None);
        assert_eq!(focus_worker(&herdr, &card(Some("w2:p1"), None)), None);
        assert_eq!(herdr.actions(), [Action::Focus("w2:p1".into())]);
    }

    #[test]
    fn focus_suggests_resume_when_the_pane_is_gone() {
        let herdr = fake(None);
        herdr.close("w2:p1");
        assert_eq!(
            focus_worker(&herdr, &card(Some("w2:p1"), None)).as_deref(),
            Some(GONE)
        );
        assert_eq!(
            focus_worker(&herdr, &card(None, None)).as_deref(),
            Some(GONE)
        );
        assert_eq!(herdr.actions(), []);
    }

    #[test]
    fn message_needs_a_pane_and_an_agent_that_isnt_blocked() {
        assert_eq!(start_message(&card(Some("w2:p1"), None)).unwrap(), "w2:p1");
        let working = with_status(card(Some("w2:p1"), None), AgentStatus::Working);
        assert_eq!(start_message(&working).unwrap(), "w2:p1");
        assert_eq!(start_message(&card(None, None)).unwrap_err(), GONE);
        let blocked = with_status(card(Some("w2:p1"), None), AgentStatus::Blocked);
        assert_eq!(start_message(&blocked).unwrap_err(), BLOCKED);
    }

    #[test]
    fn message_prompts_the_worker() {
        let herdr = fake(None);
        assert_eq!(
            send_message(&herdr, "t", "w2:p1", "  rebase first \n").as_deref(),
            Some("sent to t")
        );
        assert_eq!(send_message(&herdr, "t", "w2:p1", "   "), None);
        assert_eq!(
            herdr.actions(),
            [Action::Prompt {
                pane: "w2:p1".into(),
                text: "rebase first".into()
            }]
        );
    }

    #[test]
    fn message_to_a_blocked_or_gone_worker_says_so() {
        let herdr = fake(None);
        herdr.block("w2:p1");
        herdr.close("w2:p2");
        assert_eq!(
            send_message(&herdr, "t", "w2:p1", "hi").as_deref(),
            Some(BLOCKED)
        );
        assert_eq!(
            send_message(&herdr, "t", "w2:p2", "hi").as_deref(),
            Some(GONE)
        );
        assert_eq!(herdr.actions(), []);
    }

    #[test]
    fn opens_only_web_pr_urls() {
        let url = "https://github.com/erd/web/pull/123";
        let opened = RefCell::new(Vec::new());
        let launch = |url: &str| {
            opened.borrow_mut().push(url.to_owned());
            Ok(())
        };
        assert_eq!(open_pr(&card(None, Some(url)), launch), None);
        assert_eq!(
            open_pr(&card(None, None), launch).as_deref(),
            Some("t: no PR yet")
        );
        assert_eq!(
            open_pr(&card(None, Some("-a Calculator")), launch).as_deref(),
            Some("not a web URL: -a Calculator")
        );
        assert_eq!(*opened.borrow(), [url]);

        let failing = |_: &str| Err(io::Error::other("boom"));
        assert_eq!(
            open_pr(&card(None, Some(url)), failing).as_deref(),
            Some("couldn't open the PR: boom")
        );
    }

    fn write_record(repo: &TempRepo, pane: Option<&str>) {
        let record = RepoRecord {
            schema: SchemaV1,
            repo_root: "/src/web".into(),
            repo_id: REPO_ID.into(),
            conductor_session: "conductor-web".into(),
            herdr_workspace_id: Some("w1".into()),
            herdr_pane_id: pane.map(Into::into),
        };
        write_repo_record_atomic(&repo.repo_dir(), &record).unwrap();
    }

    #[test]
    fn conductor_focus_prefers_the_recorded_pane() {
        let repo = TempRepo::new();
        write_record(&repo, Some("w1:p1"));
        let herdr = fake(Some("w1:t1"));
        let context = herdr.plugin_context().unwrap();
        assert_eq!(focus_conductor(&herdr, &repo.repo_dir(), &context), None);
        assert_eq!(herdr.actions(), [Action::Focus("w1:p1".into())]);
    }

    #[test]
    fn conductor_focus_falls_back_to_the_decks_tab() {
        let repo = TempRepo::new();
        let herdr = fake(Some("w1:t1"));
        let context = herdr.plugin_context().unwrap();
        // No repo.json, then one without a pane, then one whose pane has closed.
        assert_eq!(focus_conductor(&herdr, &repo.repo_dir(), &context), None);
        write_record(&repo, None);
        assert_eq!(focus_conductor(&herdr, &repo.repo_dir(), &context), None);
        write_record(&repo, Some("w1:p1"));
        herdr.close("w1:p1");
        assert_eq!(focus_conductor(&herdr, &repo.repo_dir(), &context), None);
        assert_eq!(herdr.actions(), vec![Action::FocusTab("w1:t1".into()); 3]);

        write_record(&repo, None);
        let no_tab = fake(None);
        let context = no_tab.plugin_context().unwrap();
        assert_eq!(
            focus_conductor(&no_tab, &repo.repo_dir(), &context).as_deref(),
            Some("no conductor pane recorded")
        );
    }
}
