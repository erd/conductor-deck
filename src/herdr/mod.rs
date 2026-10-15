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

use std::sync::mpsc::Receiver;

pub use error::{HerdrError, Result};
pub use event::HerdrEvent;
pub use socket::SocketClient;
use socket::StopHandle;
use types::{Agent, PluginContext, Snapshot};

pub trait HerdrClient {
    /// The deck's own pane, tab and workspace.
    fn plugin_context(&self) -> Result<PluginContext>;
    fn snapshot(&self) -> Result<Snapshot>;
    /// Agent status plus its `context` and `limit` token readouts.
    fn agent_get(&self, pane: &str) -> Result<Agent>;
    fn agent_focus(&self, pane: &str) -> Result<()>;
    /// Fails with [`HerdrError::AgentBlocked`] if the agent is waiting on its own question.
    fn agent_prompt(&self, pane: &str, text: &str) -> Result<()>;
    /// `path` walks the tab's split tree from the root; `ratio` is the first child's share.
    fn set_split_ratio(&self, tab: &str, path: &[bool], ratio: f32) -> Result<()>;
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
