//! An in-memory `HerdrClient` for tests of code that drives herdr.

use std::collections::HashSet;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::types::{Agent, PluginContext, Snapshot};
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
}

pub struct FakeHerdr {
    pub context: PluginContext,
    pub snapshot: Mutex<Snapshot>,
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

    fn set_split_ratio(&self, tab: &str, path: &[bool], ratio: f32) -> Result<()> {
        self.record(Action::SetSplitRatio {
            tab: tab.to_owned(),
            path: path.to_vec(),
            ratio,
        });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::types::AgentStatus;

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
        herdr.set_split_ratio("w1:t1", &[true], 0.7).unwrap();
        assert_eq!(
            herdr.actions(),
            [
                Action::Focus("w1:p1".into()),
                Action::FocusTab("w1:t1".into()),
                Action::Prompt {
                    pane: "w1:p1".into(),
                    text: "go".into()
                },
                Action::SetSplitRatio {
                    tab: "w1:t1".into(),
                    path: vec![true],
                    ratio: 0.7
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
