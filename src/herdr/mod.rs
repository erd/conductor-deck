//! The deck's view of herdr: its socket API (NDJSON, one request per connection) and the
//! plugin context herdr passes in the environment.

pub mod error;
pub mod event;
#[cfg(test)]
pub mod fake;
pub mod socket;
#[cfg(test)]
mod test_server;
pub mod types;

use std::sync::mpsc::{Receiver, Sender};

pub use error::{HerdrError, Result};
pub use event::HerdrEvent;
pub use socket::SocketClient;
use socket::StopHandle;
use types::{Agent, Layout, LayoutTree, PluginContext, Snapshot};

pub trait HerdrClient {
    /// The deck's own pane, tab and workspace.
    fn plugin_context(&self) -> Result<PluginContext>;
    fn snapshot(&self) -> Result<Snapshot>;
    /// Agent status plus its `context` and `limit` token readouts.
    fn agent_get(&self, pane: &str) -> Result<Agent>;
    fn agent_focus(&self, pane: &str) -> Result<()>;
    /// Switches to `tab`, and to its workspace.
    fn tab_focus(&self, tab: &str) -> Result<()>;
    /// Fails with [`HerdrError::AgentBlocked`] if the agent is waiting on its own question.
    fn agent_prompt(&self, pane: &str, text: &str) -> Result<()>;
    /// The geometry of the tab holding `pane`.
    fn pane_layout(&self, pane: &str) -> Result<Layout>;
    /// The split tree of `tab`.
    fn layout_export(&self, tab: &str) -> Result<LayoutTree>;
    /// `path` walks the tab's split tree from the root; `ratio` is the first child's share.
    /// Returns the tree as herdr stored it, which clamps `ratio` into 0.1..=0.9.
    fn set_split_ratio(&self, tab: &str, path: &[bool], ratio: f32) -> Result<LayoutTree>;
    /// Opens `plugin`'s `entrypoint` pane in a split to the right of `beside`, without focusing
    /// it. Returns the new pane's id.
    fn plugin_pane_open(&self, plugin: &str, entrypoint: &str, beside: &str) -> Result<String>;
    fn plugin_pane_focus(&self, pane: &str) -> Result<()>;
    fn plugin_pane_close(&self, pane: &str) -> Result<()>;
    /// Streams pane, layout and workspace events, plus agent status changes for `watch_panes`
    /// (herdr only reports those per pane). The stream starts with [`HerdrEvent::Resync`], and
    /// resyncs again after it falls behind or reconnects. To change the watched panes, drop the
    /// subscription and subscribe again.
    fn subscribe(&self, watch_panes: &[String]) -> Result<Subscription>;
}

/// A live event stream. Dropping it stops the background reader.
pub struct Subscription {
    pub events: Receiver<HerdrEvent>,
    _stop: Option<StopHandle>,
}

impl Subscription {
    /// Sends every event to `tx`, wrapped by `wrap`, from a background thread, so one channel
    /// can carry herdr events alongside others. Dropping the returned [`Forwarding`] stops the
    /// subscription, and the thread ends once the stream does or `tx`'s receiver is gone.
    pub fn forward<T: Send + 'static>(
        self,
        tx: Sender<T>,
        wrap: impl Fn(HerdrEvent) -> T + Send + 'static,
    ) -> std::io::Result<Forwarding> {
        let Subscription {
            events,
            _stop: stop,
        } = self;
        std::thread::Builder::new()
            .name("herdr-forward".into())
            .spawn(move || {
                for event in events {
                    if tx.send(wrap(event)).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Forwarding { _stop: stop })
    }
}

/// Keeps a forwarded [`Subscription`] running until dropped.
pub struct Forwarding {
    _stop: Option<StopHandle>,
}
