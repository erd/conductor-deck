//! The `toggle` action: closes the deck in the current tab, or opens one beside the pane it was
//! run from. It only ever closes panes listed in the plugin's `panes` state file, which every
//! deck adds itself to, so a user's own pane is never closed.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use crate::herdr::types::PluginContext;
use crate::herdr::{HerdrClient, HerdrError};

/// The plugin and pane entrypoint ids from `herdr-plugin.toml`.
pub const PLUGIN_ID: &str = "conductor-deck";
pub const ENTRYPOINT: &str = "deck";
/// The deck pane's manifest title, which herdr shows as the pane's label.
pub const DECK_TITLE: &str = "conductor";

#[derive(Debug, thiserror::Error)]
pub enum ToggleError {
    #[error(transparent)]
    Herdr(#[from] HerdrError),
    #[error("deck pane list: {0}")]
    Io(#[from] io::Error),
    #[error("pane {0} is not in herdr's session")]
    UnknownPane(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Opened(String),
    Closed(String),
    /// A deck the plugin didn't record, such as one from an older deck version. It is focused
    /// rather than closed, and rather than opening a second one.
    Focused(String),
}

/// The deck pane ids recorded in `$HERDR_PLUGIN_STATE_DIR/panes`, one per line.
pub struct Panes {
    path: PathBuf,
}

impl Panes {
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            path: dir.join("panes"),
        }
    }

    pub fn from_env() -> Result<Self, HerdrError> {
        std::env::var_os("HERDR_PLUGIN_STATE_DIR")
            .filter(|dir| !dir.is_empty())
            .map(|dir| Self::in_dir(Path::new(&dir)))
            .ok_or(HerdrError::MissingEnv("HERDR_PLUGIN_STATE_DIR"))
    }

    pub fn read(&self) -> io::Result<Vec<String>> {
        match fs::read_to_string(&self.path) {
            Ok(text) => Ok(text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(Vec::new()),
            Err(err) => Err(err),
        }
    }

    pub fn record(&self, pane: &str) -> io::Result<()> {
        let mut panes = self.read()?;
        if panes.iter().any(|p| p == pane) {
            return Ok(());
        }
        panes.push(pane.to_owned());
        self.write(&panes)
    }

    pub fn forget(&self, gone: &[String]) -> io::Result<()> {
        let mut panes = self.read()?;
        let before = panes.len();
        panes.retain(|p| !gone.contains(p));
        if panes.len() == before {
            return Ok(());
        }
        self.write(&panes)
    }

    /// Replaces the file in one rename, so a reader never sees half a list.
    fn write(&self, panes: &[String]) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self
            .path
            .with_file_name(format!("panes.{}.tmp", std::process::id()));
        let mut text = panes.join("\n");
        text.push('\n');
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &self.path)
    }
}

/// The pane the action was run from: `HERDR_PANE_ID`, or the focused pane from the context.
pub fn invoked_from() -> Result<String, HerdrError> {
    let var = |name| std::env::var(name).ok().filter(|v: &String| !v.is_empty());
    let pane = var("HERDR_PANE_ID");
    let context = PluginContext::from_parts(
        var("HERDR_PLUGIN_CONTEXT_JSON").as_deref(),
        Some(pane.clone().unwrap_or_default()),
        None,
        None,
    )?;
    pane.or(context.focused_pane_id)
        .ok_or(HerdrError::MissingEnv("HERDR_PANE_ID"))
}

/// Closes the recorded deck in `target`'s tab; otherwise focuses an unrecorded deck there;
/// otherwise opens a deck beside `target` and records it.
pub fn toggle(
    herdr: &impl HerdrClient,
    panes: &Panes,
    target: &str,
) -> Result<Outcome, ToggleError> {
    let snapshot = herdr.snapshot()?;
    let tab = snapshot
        .panes
        .iter()
        .find(|p| p.pane_id == target)
        .map(|p| p.tab_id.clone())
        .ok_or_else(|| ToggleError::UnknownPane(target.to_owned()))?;

    let (live, gone): (Vec<String>, Vec<String>) = panes
        .read()?
        .into_iter()
        .partition(|id| snapshot.panes.iter().any(|p| &p.pane_id == id));
    panes.forget(&gone)?;

    let in_tab = |id: &str| {
        snapshot
            .panes
            .iter()
            .any(|p| p.pane_id == id && p.tab_id == tab)
    };
    if let Some(deck) = live.iter().find(|id| in_tab(id)) {
        herdr.plugin_pane_close(deck)?;
        panes.forget(std::slice::from_ref(deck))?;
        return Ok(Outcome::Closed(deck.clone()));
    }
    if let Some(deck) = snapshot
        .panes
        .iter()
        .find(|p| p.tab_id == tab && p.label.as_deref() == Some(DECK_TITLE))
    {
        herdr.plugin_pane_focus(&deck.pane_id)?;
        return Ok(Outcome::Focused(deck.pane_id.clone()));
    }
    let deck = herdr.plugin_pane_open(PLUGIN_ID, ENTRYPOINT, target)?;
    // The new deck records itself too; recording it here covers the moments before it starts.
    panes.record(&deck)?;
    Ok(Outcome::Opened(deck))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::fake::{Action, FakeHerdr};
    use crate::herdr::types::{AgentStatus, Pane, Snapshot, Tokens};

    fn pane(id: &str, tab: &str, label: Option<&str>) -> Pane {
        Pane {
            pane_id: id.into(),
            tab_id: tab.into(),
            workspace_id: "w1".into(),
            agent: None,
            status: AgentStatus::Unknown,
            focused: false,
            label: label.map(str::to_owned),
            tokens: Tokens::default(),
            cwd: None,
        }
    }

    fn fake(panes: Vec<Pane>) -> FakeHerdr {
        let context = PluginContext::from_parts(None, Some("w1:c".into()), None, None).unwrap();
        FakeHerdr::new(
            context,
            Snapshot {
                panes,
                ..Snapshot::default()
            },
        )
    }

    fn state() -> (tempfile::TempDir, Panes) {
        let dir = tempfile::tempdir().unwrap();
        let panes = Panes::in_dir(&dir.path().join("state"));
        (dir, panes)
    }

    #[test]
    fn toggling_twice_opens_then_closes_only_the_deck() {
        let herdr = fake(vec![
            pane("w1:c", "w1:t1", Some("claude")),
            pane("w1:mine", "w1:t1", None),
        ]);
        let (_dir, panes) = state();

        let Outcome::Opened(deck) = toggle(&herdr, &panes, "w1:c").unwrap() else {
            panic!("expected the deck to open");
        };
        assert_eq!(panes.read().unwrap(), std::slice::from_ref(&deck));
        assert_eq!(
            toggle(&herdr, &panes, "w1:c").unwrap(),
            Outcome::Closed(deck.clone())
        );
        assert_eq!(panes.read().unwrap(), Vec::<String>::new());

        assert_eq!(
            herdr.actions(),
            [
                Action::OpenPluginPane {
                    plugin: "conductor-deck".into(),
                    entrypoint: "deck".into(),
                    beside: "w1:c".into(),
                },
                Action::ClosePluginPane(deck),
            ]
        );
        let left: Vec<_> = herdr.snapshot().unwrap().panes;
        assert_eq!(left.len(), 2);
    }

    #[test]
    fn closes_a_deck_that_recorded_itself_from_any_pane_in_the_tab() {
        let herdr = fake(vec![
            pane("w1:c", "w1:t1", None),
            pane("w1:other", "w1:t1", None),
            pane("w1:deck", "w1:t1", Some(DECK_TITLE)),
        ]);
        let (_dir, panes) = state();
        panes.record("w1:deck").unwrap();
        assert_eq!(
            toggle(&herdr, &panes, "w1:other").unwrap(),
            Outcome::Closed("w1:deck".into())
        );
    }

    #[test]
    fn focuses_an_unrecorded_deck_instead_of_opening_another() {
        let herdr = fake(vec![
            pane("w1:c", "w1:t1", None),
            pane("w1:old", "w1:t1", Some(DECK_TITLE)),
        ]);
        let (_dir, panes) = state();
        assert_eq!(
            toggle(&herdr, &panes, "w1:c").unwrap(),
            Outcome::Focused("w1:old".into())
        );
        assert_eq!(herdr.actions(), [Action::FocusPluginPane("w1:old".into())]);
        assert_eq!(panes.read().unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_deck_in_another_tab_does_not_count() {
        let herdr = fake(vec![
            pane("w1:c", "w1:t1", None),
            pane("w2:deck", "w2:t1", Some(DECK_TITLE)),
        ]);
        let (_dir, panes) = state();
        panes.record("w2:deck").unwrap();
        assert!(matches!(
            toggle(&herdr, &panes, "w1:c").unwrap(),
            Outcome::Opened(_)
        ));
        assert_eq!(panes.read().unwrap().len(), 2);
    }

    #[test]
    fn prunes_ids_of_closed_panes() {
        let herdr = fake(vec![pane("w1:c", "w1:t1", None)]);
        let (_dir, panes) = state();
        panes.record("w1:gone").unwrap();
        assert!(matches!(
            toggle(&herdr, &panes, "w1:c").unwrap(),
            Outcome::Opened(_)
        ));
        assert!(!panes.read().unwrap().contains(&"w1:gone".to_owned()));
    }

    #[test]
    fn unknown_target_is_an_error() {
        let herdr = fake(vec![]);
        let (_dir, panes) = state();
        assert!(matches!(
            toggle(&herdr, &panes, "w1:nope"),
            Err(ToggleError::UnknownPane(_))
        ));
    }

    #[test]
    fn pane_list_round_trips() {
        let (_dir, panes) = state();
        assert_eq!(panes.read().unwrap(), Vec::<String>::new());
        panes.record("a").unwrap();
        panes.record("b").unwrap();
        panes.record("a").unwrap();
        assert_eq!(panes.read().unwrap(), ["a", "b"]);
        panes.forget(&["a".into(), "x".into()]).unwrap();
        assert_eq!(panes.read().unwrap(), ["b"]);
    }
}
