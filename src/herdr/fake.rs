//! An in-memory `HerdrClient` for tests of code that drives herdr.

use std::collections::HashSet;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::types::{
    Agent, AgentStatus, Layout, LayoutNode, LayoutPane, LayoutTree, Pane, PluginContext, Rect,
    Snapshot, SplitDirection, Tokens,
};
use super::{HerdrClient, HerdrError, HerdrEvent, Result, Subscription};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Focus(String),
    FocusTab(String),
    Prompt {
        pane: String,
        text: String,
    },
    SetSplitRatio {
        tab: String,
        path: Vec<bool>,
        ratio: f32,
    },
    OpenPluginPane {
        plugin: String,
        entrypoint: String,
        beside: String,
    },
    FocusPluginPane(String),
    ClosePluginPane(String),
}

/// One tab's split tree and the area it fills; the fake derives pane geometry from both.
#[derive(Debug, Clone)]
pub struct FakeTab {
    pub tree: LayoutTree,
    pub area: Rect,
}

pub struct FakeHerdr {
    pub context: PluginContext,
    pub snapshot: Mutex<Snapshot>,
    pub tabs: Mutex<Vec<FakeTab>>,
    actions: Mutex<Vec<Action>>,
    blocked: Mutex<HashSet<String>>,
    gone: Mutex<HashSet<String>>,
    subscribers: Mutex<Vec<Sender<HerdrEvent>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FakeHerdr {
    pub fn new(context: PluginContext, snapshot: Snapshot) -> Self {
        Self {
            context,
            snapshot: Mutex::new(snapshot),
            tabs: Mutex::default(),
            actions: Mutex::default(),
            blocked: Mutex::default(),
            gone: Mutex::default(),
            subscribers: Mutex::default(),
        }
    }

    /// Makes prompts to `pane` fail with `AgentBlocked`, as herdr does for a blocked agent.
    pub fn block(&self, pane: &str) {
        lock(&self.blocked).insert(pane.to_owned());
    }

    /// Makes focusing or prompting `pane` fail with `NotFound`, as herdr does once the pane
    /// has closed.
    pub fn close(&self, pane: &str) {
        lock(&self.gone).insert(pane.to_owned());
    }

    pub fn actions(&self) -> Vec<Action> {
        lock(&self.actions).clone()
    }

    /// Delivers `event` to every live subscription.
    pub fn emit(&self, event: &HerdrEvent) {
        lock(&self.subscribers).retain(|tx| tx.send(event.clone()).is_ok());
    }

    /// Adds a tab whose layout the fake reports and updates.
    pub fn add_tab(&self, root: LayoutNode, tab: &str, width: u16) {
        lock(&self.tabs).push(FakeTab {
            tree: LayoutTree {
                tab_id: tab.to_owned(),
                zoomed: false,
                root,
            },
            area: Rect {
                x: 0,
                y: 0,
                width,
                height: 40,
            },
        });
    }

    /// Runs `change` on `tab`, as a user dragging a divider or resizing the terminal would.
    pub fn edit_tab(&self, tab: &str, change: impl FnOnce(&mut FakeTab)) {
        let mut tabs = lock(&self.tabs);
        change(
            tabs.iter_mut()
                .find(|t| t.tree.tab_id == tab)
                .expect("tab exists"),
        );
    }

    fn record(&self, action: Action) {
        lock(&self.actions).push(action);
    }

    fn check_present(&self, pane: &str) -> Result<()> {
        if lock(&self.gone).contains(pane) {
            return Err(HerdrError::NotFound {
                code: "pane_not_found".into(),
                message: format!("pane {pane} not found"),
            });
        }
        Ok(())
    }
}

impl HerdrClient for FakeHerdr {
    fn plugin_context(&self) -> Result<PluginContext> {
        Ok(self.context.clone())
    }

    fn snapshot(&self) -> Result<Snapshot> {
        Ok(lock(&self.snapshot).clone())
    }

    fn agent_get(&self, pane: &str) -> Result<Agent> {
        lock(&self.snapshot)
            .agents
            .iter()
            .find(|agent| agent.pane_id == pane)
            .cloned()
            .ok_or_else(|| HerdrError::NotFound {
                code: "agent_not_found".into(),
                message: format!("agent target {pane} not found"),
            })
    }

    fn agent_focus(&self, pane: &str) -> Result<()> {
        self.check_present(pane)?;
        self.record(Action::Focus(pane.to_owned()));
        Ok(())
    }

    fn tab_focus(&self, tab: &str) -> Result<()> {
        self.record(Action::FocusTab(tab.to_owned()));
        Ok(())
    }

    fn agent_prompt(&self, pane: &str, text: &str) -> Result<()> {
        self.check_present(pane)?;
        if lock(&self.blocked).contains(pane) {
            return Err(HerdrError::AgentBlocked(format!("agent {pane} is blocked")));
        }
        self.record(Action::Prompt {
            pane: pane.to_owned(),
            text: text.to_owned(),
        });
        Ok(())
    }

    fn pane_layout(&self, pane: &str) -> Result<Layout> {
        lock(&self.tabs)
            .iter()
            .find_map(|tab| {
                let mut panes = Vec::new();
                place(&tab.tree.root, tab.area, &mut panes);
                panes.iter().any(|p| p.pane_id == pane).then(|| Layout {
                    workspace_id: workspace_of(&tab.tree.tab_id),
                    tab_id: tab.tree.tab_id.clone(),
                    zoomed: tab.tree.zoomed,
                    area: tab.area,
                    panes,
                    splits: Vec::new(),
                })
            })
            .ok_or_else(|| not_found(pane))
    }

    fn layout_export(&self, tab: &str) -> Result<LayoutTree> {
        lock(&self.tabs)
            .iter()
            .find(|t| t.tree.tab_id == tab)
            .map(|t| t.tree.clone())
            .ok_or_else(|| not_found(tab))
    }

    fn set_split_ratio(&self, tab: &str, path: &[bool], ratio: f32) -> Result<LayoutTree> {
        self.record(Action::SetSplitRatio {
            tab: tab.to_owned(),
            path: path.to_vec(),
            ratio,
        });
        let mut tabs = lock(&self.tabs);
        let tree = &mut tabs
            .iter_mut()
            .find(|t| t.tree.tab_id == tab)
            .ok_or_else(|| not_found(tab))?
            .tree;
        let mut node = &mut tree.root;
        for &second in path {
            match node {
                LayoutNode::Split {
                    first, second: s, ..
                } => node = if second { s } else { first },
                LayoutNode::Pane { .. } => return Err(not_found("split")),
            }
        }
        let LayoutNode::Split { ratio: stored, .. } = node else {
            return Err(not_found("split"));
        };
        *stored = ratio.clamp(0.1, 0.9);
        Ok(tree.clone())
    }

    fn plugin_pane_open(&self, plugin: &str, entrypoint: &str, beside: &str) -> Result<String> {
        self.check_present(beside)?;
        self.record(Action::OpenPluginPane {
            plugin: plugin.to_owned(),
            entrypoint: entrypoint.to_owned(),
            beside: beside.to_owned(),
        });
        let mut snapshot = lock(&self.snapshot);
        let target = snapshot
            .panes
            .iter()
            .find(|p| p.pane_id == beside)
            .cloned()
            .ok_or_else(|| not_found(beside))?;
        let pane_id = format!("{}:p{}", target.workspace_id, 100 + snapshot.panes.len());
        snapshot.panes.push(Pane {
            pane_id: pane_id.clone(),
            label: Some("conductor".into()),
            agent: None,
            status: AgentStatus::Unknown,
            focused: false,
            tokens: Tokens::default(),
            cwd: None,
            ..target
        });
        Ok(pane_id)
    }

    fn plugin_pane_focus(&self, pane: &str) -> Result<()> {
        self.check_present(pane)?;
        self.record(Action::FocusPluginPane(pane.to_owned()));
        Ok(())
    }

    fn plugin_pane_close(&self, pane: &str) -> Result<()> {
        self.check_present(pane)?;
        self.record(Action::ClosePluginPane(pane.to_owned()));
        lock(&self.snapshot).panes.retain(|p| p.pane_id != pane);
        Ok(())
    }

    fn subscribe(&self, _watch_panes: &[String]) -> Result<Subscription> {
        let (tx, events) = mpsc::channel();
        // Like the socket client, every stream opens with a resync.
        tx.send(HerdrEvent::Resync(self.snapshot()?))
            .expect("receiver is held below");
        lock(&self.subscribers).push(tx);
        Ok(Subscription {
            events,
            _stop: None,
        })
    }
}

fn not_found(what: &str) -> HerdrError {
    HerdrError::NotFound {
        code: "not_found".into(),
        message: format!("{what} not found"),
    }
}

fn workspace_of(tab: &str) -> String {
    tab.split(':').next().unwrap_or_default().to_owned()
}

/// Lays `node` out over `area` the way herdr 0.9.5 does: the first child of a split gets
/// `round(ratio × size)` cells and the second gets the rest, with no divider cell.
fn place(node: &LayoutNode, area: Rect, out: &mut Vec<LayoutPane>) {
    match node {
        LayoutNode::Pane { pane_id } => out.push(LayoutPane {
            pane_id: pane_id.clone().unwrap_or_default(),
            rect: area,
        }),
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let size = match direction {
                SplitDirection::Right => area.width,
                SplitDirection::Down => area.height,
            };
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a share of a u16 fits in a u16"
            )]
            let cut = (ratio * f32::from(size)).round() as u16;
            let (a, b) = match direction {
                SplitDirection::Right => (
                    Rect { width: cut, ..area },
                    Rect {
                        x: area.x + cut,
                        width: area.width - cut,
                        ..area
                    },
                ),
                SplitDirection::Down => (
                    Rect {
                        height: cut,
                        ..area
                    },
                    Rect {
                        y: area.y + cut,
                        height: area.height - cut,
                        ..area
                    },
                ),
            };
            place(first, a, out);
            place(second, b, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake() -> FakeHerdr {
        let context = PluginContext::from_parts(None, Some("w1:p9".into()), None, None).unwrap();
        FakeHerdr::new(context, Snapshot::default())
    }

    #[test]
    fn records_actions_and_refuses_blocked_prompts() {
        let herdr = fake();
        herdr.block("w1:p2");
        herdr.close("w1:p3");
        herdr.agent_focus("w1:p1").unwrap();
        herdr.tab_focus("w1:t1").unwrap();
        herdr.agent_prompt("w1:p1", "go").unwrap();
        assert!(matches!(
            herdr.agent_prompt("w1:p2", "go"),
            Err(HerdrError::AgentBlocked(_))
        ));
        for result in [
            herdr.agent_focus("w1:p3"),
            herdr.agent_prompt("w1:p3", "go"),
        ] {
            assert!(matches!(result, Err(HerdrError::NotFound { .. })));
        }
        assert_eq!(
            herdr.actions(),
            [
                Action::Focus("w1:p1".into()),
                Action::FocusTab("w1:t1".into()),
                Action::Prompt {
                    pane: "w1:p1".into(),
                    text: "go".into()
                },
            ]
        );
        assert!(matches!(
            herdr.agent_get("w1:p1"),
            Err(HerdrError::NotFound { .. })
        ));
        assert_eq!(herdr.plugin_context().unwrap().pane_id, "w1:p9");
    }

    #[test]
    fn subscription_resyncs_then_receives_emitted_events() {
        let herdr = fake();
        let sub = herdr.subscribe(&[]).unwrap();
        let event = HerdrEvent::AgentStatusChanged {
            pane_id: "w1:p1".into(),
            workspace_id: "w1".into(),
            status: AgentStatus::Blocked,
        };
        herdr.emit(&event);
        assert_eq!(
            sub.events.recv().unwrap(),
            HerdrEvent::Resync(Snapshot::default())
        );
        assert_eq!(sub.events.recv().unwrap(), event);
    }

    #[test]
    fn forwards_wrapped_events() {
        let herdr = fake();
        let (tx, rx) = mpsc::channel();
        let _forwarding = herdr
            .subscribe(&[])
            .unwrap()
            .forward(tx, |event| ("herdr", event))
            .unwrap();
        assert_eq!(
            rx.recv().unwrap(),
            ("herdr", HerdrEvent::Resync(Snapshot::default()))
        );
        herdr.emit(&HerdrEvent::Disconnected);
        assert_eq!(rx.recv().unwrap(), ("herdr", HerdrEvent::Disconnected));
    }
}
