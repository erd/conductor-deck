use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::error::{HerdrError, Result};
use super::event::{AGENT_STATUS_SUBSCRIPTION, GLOBAL_SUBSCRIPTIONS, HerdrEvent, decode_event};
use super::types::{Agent, Layout, LayoutTree, PluginContext, Snapshot};
use super::{HerdrClient, Subscription};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the subscription waits between reconnect attempts.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(100),
            max: Duration::from_secs(5),
        }
    }
}

/// Talks to herdr over its Unix socket. herdr answers one request per connection, so every call
/// opens a fresh connection; only a subscription holds one open.
pub struct SocketClient {
    conn: Conn,
    backoff: Backoff,
}

impl SocketClient {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            conn: Conn {
                path: path.into(),
                next_id: Arc::new(AtomicU64::new(1)),
            },
            backoff: Backoff::default(),
        }
    }

    pub fn from_env() -> Result<Self> {
        let path = std::env::var_os("HERDR_SOCKET_PATH")
            .filter(|p| !p.is_empty())
            .ok_or(HerdrError::MissingEnv("HERDR_SOCKET_PATH"))?;
        Ok(Self::new(path))
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self
    }
}

impl HerdrClient for SocketClient {
    fn plugin_context(&self) -> Result<PluginContext> {
        PluginContext::from_env()
    }

    fn snapshot(&self) -> Result<Snapshot> {
        self.conn.snapshot()
    }

    fn agent_get(&self, pane: &str) -> Result<Agent> {
        let result = self.conn.request("agent.get", &json!({ "target": pane }))?;
        decode_result(result, "agent_info", "agent")
    }

    fn agent_focus(&self, pane: &str) -> Result<()> {
        self.conn
            .request("agent.focus", &json!({ "target": pane }))?;
        Ok(())
    }

    fn tab_focus(&self, tab: &str) -> Result<()> {
        self.conn.request("tab.focus", &json!({ "tab_id": tab }))?;
        Ok(())
    }

    fn agent_prompt(&self, pane: &str, text: &str) -> Result<()> {
        let result = self
            .conn
            .request("agent.prompt", &json!({ "target": pane, "text": text }))?;
        expect_type(&result, "agent_prompted")
    }

    fn pane_layout(&self, pane: &str) -> Result<Layout> {
        let result = self
            .conn
            .request("pane.layout", &json!({ "pane_id": pane }))?;
        decode_result(result, "pane_layout", "layout")
    }

    fn layout_export(&self, tab: &str) -> Result<LayoutTree> {
        let result = self
            .conn
            .request("layout.export", &json!({ "tab_id": tab }))?;
        decode_result(result, "layout_export", "layout")
    }

    fn set_split_ratio(&self, tab: &str, path: &[bool], ratio: f32) -> Result<LayoutTree> {
        let params = json!({ "tab_id": tab, "path": path, "ratio": ratio });
        let result = self.conn.request("layout.set_split_ratio", &params)?;
        decode_result(result, "layout_split_ratio_set", "layout")
    }

    fn plugin_pane_open(&self, plugin: &str, entrypoint: &str, beside: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct Opened {
            pane: PaneId,
        }
        #[derive(Deserialize)]
        struct PaneId {
            pane_id: String,
        }
        let params = json!({
            "plugin_id": plugin,
            "entrypoint": entrypoint,
            "placement": "split",
            "direction": "right",
            "target_pane_id": beside,
        });
        let result = self.conn.request("plugin.pane.open", &params)?;
        let opened: Opened = decode_result(result, "plugin_pane_opened", "plugin_pane")?;
        Ok(opened.pane.pane_id)
    }

    fn plugin_pane_focus(&self, pane: &str) -> Result<()> {
        let result = self
            .conn
            .request("plugin.pane.focus", &json!({ "pane_id": pane }))?;
        expect_type(&result, "plugin_pane_focused")
    }

    fn plugin_pane_close(&self, pane: &str) -> Result<()> {
        let result = self
            .conn
            .request("plugin.pane.close", &json!({ "pane_id": pane }))?;
        expect_type(&result, "plugin_pane_closed")
    }

    fn subscribe(&self, watch_panes: &[String]) -> Result<Subscription> {
        let (tx, events) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let current = Arc::new(Mutex::new(Current::default()));
        let mut watcher = Watcher {
            conn: self.conn.clone(),
            panes: watch_panes.to_vec(),
            tx,
            stop_rx,
            current: Arc::clone(&current),
            backoff: self.backoff,
        };
        // The first connection is made here so a missing or refused socket reaches the caller.
        let reader = watcher.connect().map_err(|halt| match halt {
            Halt::Failed(err) => err,
            Halt::Stopped => HerdrError::protocol("subscription stopped while starting"),
        })?;
        std::thread::Builder::new()
            .name("herdr-events".into())
            .spawn(move || watcher.run(reader))?;
        Ok(Subscription {
            events,
            _stop: Some(StopHandle {
                current,
                _stop_tx: stop_tx,
            }),
        })
    }
}

/// Stops a socket subscription when dropped: wakes a reconnect sleep and unblocks the read.
pub struct StopHandle {
    current: Arc<Mutex<Current>>,
    _stop_tx: Sender<()>,
}

impl Drop for StopHandle {
    fn drop(&mut self) {
        let mut current = lock(&self.current);
        current.stopped = true;
        if let Some(stream) = current.stream.take() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// The subscription stream being read, shared with the `StopHandle` so it can be shut down.
#[derive(Default)]
struct Current {
    stopped: bool,
    stream: Option<UnixStream>,
}

fn lock(current: &Mutex<Current>) -> std::sync::MutexGuard<'_, Current> {
    current
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone)]
struct Conn {
    path: PathBuf,
    next_id: Arc<AtomicU64>,
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    id: String,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
}

impl Conn {
    /// Opens a connection and writes one request line. Returns the request id to match.
    fn send(&self, method: &str, params: &Value) -> Result<(BufReader<UnixStream>, String)> {
        let mut stream = UnixStream::connect(&self.path)?;
        stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
        stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
        let id = format!("deck:{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let mut line =
            serde_json::to_vec(&json!({ "id": id, "method": method, "params": params }))?;
        line.push(b'\n');
        stream.write_all(&line)?;
        Ok((BufReader::new(stream), id))
    }

    fn request(&self, method: &str, params: &Value) -> Result<Value> {
        let (mut reader, id) = self.send(method, params)?;
        read_response(&mut reader, &id)
    }

    fn snapshot(&self) -> Result<Snapshot> {
        let result = self.request("session.snapshot", &json!({}))?;
        decode_result(result, "session_snapshot", "snapshot")
    }
}

/// Reads one response line and returns its `result`, or its `error` as a typed error.
fn read_response(reader: &mut impl BufRead, id: &str) -> Result<Value> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(HerdrError::protocol("connection closed before a response"));
    }
    let response: Response = serde_json::from_str(&line)?;
    // herdr answers a request it could not parse with an empty id.
    if response.id != id && !(response.id.is_empty() && response.error.is_some()) {
        return Err(HerdrError::protocol(format!(
            "response id {:?} does not match request {id:?}",
            response.id
        )));
    }
    match (response.result, response.error) {
        (_, Some(err)) => Err(HerdrError::from_code(err.code, err.message)),
        (Some(result), None) => Ok(result),
        (None, None) => Err(HerdrError::protocol(
            "response has neither result nor error",
        )),
    }
}

fn expect_type(result: &Value, expected: &str) -> Result<()> {
    match result.get("type").and_then(Value::as_str) {
        Some(kind) if kind == expected => Ok(()),
        other => Err(HerdrError::protocol(format!(
            "expected a {expected} result, got {other:?}"
        ))),
    }
}

fn decode_result<T: serde::de::DeserializeOwned>(
    mut result: Value,
    expected: &str,
    field: &str,
) -> Result<T> {
    expect_type(&result, expected)?;
    let value = result
        .get_mut(field)
        .map(Value::take)
        .ok_or_else(|| HerdrError::protocol(format!("{expected} result has no {field}")))?;
    Ok(serde_json::from_value(value)?)
}

/// Why the subscription thread stopped reading or connecting.
enum Halt {
    /// The `Subscription` was dropped.
    Stopped,
    Failed(HerdrError),
}

impl From<HerdrError> for Halt {
    fn from(err: HerdrError) -> Self {
        Self::Failed(err)
    }
}

enum StreamEnd {
    Stopped,
    EventsLost,
    Dropped,
}

/// Owns the subscription connection on a background thread.
struct Watcher {
    conn: Conn,
    panes: Vec<String>,
    tx: Sender<HerdrEvent>,
    stop_rx: mpsc::Receiver<()>,
    current: Arc<Mutex<Current>>,
    backoff: Backoff,
}

impl Watcher {
    fn run(mut self, mut reader: BufReader<UnixStream>) {
        loop {
            match self.stream(&mut reader) {
                StreamEnd::Stopped => return,
                // herdr closes the stream after events_lost; resubscribe straight away.
                StreamEnd::EventsLost => {}
                StreamEnd::Dropped => {
                    if self.tx.send(HerdrEvent::Disconnected).is_err()
                        || !self.sleep(self.backoff.initial)
                    {
                        return;
                    }
                }
            }
            match self.reconnect() {
                Some(next) => reader = next,
                None => return,
            }
        }
    }

    /// Forwards events until the stream ends.
    fn stream(&self, reader: &mut BufReader<UnixStream>) -> StreamEnd {
        let mut line = Vec::new();
        loop {
            // `read_until` keeps a partial line across a timeout, even mid-way through a
            // multibyte character, so the next read picks up where this one stopped.
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) if self.stopped() => return StreamEnd::Stopped,
                Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    continue;
                }
                Ok(0) | Err(_) => return StreamEnd::Dropped,
                Ok(_) => {}
            }
            let text = String::from_utf8_lossy(&line).into_owned();
            line.clear();
            if let Ok(Response {
                error: Some(err), ..
            }) = serde_json::from_str(&text)
            {
                return match HerdrError::from_code(err.code, err.message) {
                    HerdrError::EventsLost => StreamEnd::EventsLost,
                    _ => StreamEnd::Dropped,
                };
            }
            // A line that fails to decode is skipped; the next resync corrects any state it carried.
            if let Ok(Some(event)) = decode_event(&text) {
                if self.tx.send(event).is_err() {
                    return StreamEnd::Stopped;
                }
            }
        }
    }

    /// Retries `connect` with exponential backoff until it succeeds or the subscription stops.
    fn reconnect(&mut self) -> Option<BufReader<UnixStream>> {
        let mut delay = self.backoff.initial;
        loop {
            match self.connect() {
                Ok(reader) => return Some(reader),
                Err(Halt::Stopped) => return None,
                Err(Halt::Failed(_)) => {
                    if !self.sleep(delay) {
                        return None;
                    }
                    delay = (delay * 2).min(self.backoff.max);
                }
            }
        }
    }

    /// Subscribes, then resyncs from a snapshot taken after the subscription started, so no
    /// change falls between the two.
    fn connect(&mut self) -> std::result::Result<BufReader<UnixStream>, Halt> {
        let reader = self.subscribe()?;
        let snapshot = self.conn.snapshot()?;
        self.tx
            .send(HerdrEvent::Resync(snapshot))
            .map_err(|_| Halt::Stopped)?;
        Ok(reader)
    }

    fn subscribe(&mut self) -> std::result::Result<BufReader<UnixStream>, Halt> {
        loop {
            match self.try_subscribe() {
                // herdr rejects the whole subscription if any watched pane is gone. Drop the dead
                // panes and retry; the resync that follows tells the caller they are gone.
                Err(Halt::Failed(HerdrError::NotFound { code, message }))
                    if code == "pane_not_found" =>
                {
                    let snapshot = self.conn.snapshot()?;
                    let before = self.panes.len();
                    self.panes
                        .retain(|pane| snapshot.panes.iter().any(|p| &p.pane_id == pane));
                    if self.panes.len() == before {
                        return Err(HerdrError::NotFound { code, message }.into());
                    }
                }
                other => return other,
            }
        }
    }

    fn try_subscribe(&self) -> std::result::Result<BufReader<UnixStream>, Halt> {
        let subscriptions: Vec<Value> = GLOBAL_SUBSCRIPTIONS
            .iter()
            .map(|kind| json!({ "type": kind }))
            .chain(
                self.panes
                    .iter()
                    .map(|pane| json!({ "type": AGENT_STATUS_SUBSCRIPTION, "pane_id": pane })),
            )
            .collect();
        let (mut reader, id) = self.conn.send(
            "events.subscribe",
            &json!({ "subscriptions": subscriptions }),
        )?;
        let ack = read_response(&mut reader, &id)?;
        expect_type(&ack, "subscription_started")?;
        // The request timeout stays set: `stream` treats it as an idle tick. Clearing it fails
        // with EINVAL on macOS if herdr has already closed its end (it does after `events_lost`),
        // and the lines it wrote before closing still need reading.

        let mut current = lock(&self.current);
        if current.stopped {
            return Err(Halt::Stopped);
        }
        current.stream = Some(reader.get_ref().try_clone().map_err(HerdrError::from)?);
        Ok(reader)
    }

    /// Sleeps for `delay`, returning false if the subscription was dropped meanwhile.
    fn sleep(&self, delay: Duration) -> bool {
        matches!(
            self.stop_rx.recv_timeout(delay),
            Err(RecvTimeoutError::Timeout)
        )
    }

    fn stopped(&self) -> bool {
        lock(&self.current).stopped
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::herdr::test_server::{
        TestServer, error_line, respond, respond_error, respond_raw, snapshot_result, socket_path,
        subscription,
    };
    use crate::herdr::types::{AgentStatus, LayoutNode, SplitDirection};

    const WAIT: Duration = Duration::from_secs(5);

    fn fast() -> Backoff {
        Backoff {
            initial: Duration::from_millis(1),
            max: Duration::from_millis(20),
        }
    }

    fn client(server: &TestServer) -> SocketClient {
        SocketClient::new(&server.path).with_backoff(fast())
    }

    fn next(sub: &Subscription) -> HerdrEvent {
        sub.events.recv_timeout(WAIT).expect("event")
    }

    fn resync_panes(event: &HerdrEvent) -> Vec<String> {
        let HerdrEvent::Resync(snapshot) = event else {
            panic!("expected Resync, got {event:?}");
        };
        snapshot.panes.iter().map(|p| p.pane_id.clone()).collect()
    }

    fn pane_updated(_: &Value) -> String {
        concat!(
            r#"{"event":"pane_updated","data":{"type":"pane_updated","pane":{"pane_id":"w1:p1","#,
            r#""terminal_id":"t","workspace_id":"w1","tab_id":"w1:t1","focused":false,"#,
            r#""agent_status":"working","revision":2,"state_labels":null}}}"#,
            "\n"
        )
        .to_owned()
    }

    fn events_lost(request: &Value) -> String {
        error_line(request, "events_lost")
    }

    #[test]
    fn frames_one_request_line_and_decodes_agent() {
        let server = TestServer::start(vec![respond(json!({
            "type": "agent_info",
            "agent": { "pane_id": "w1:p1", "terminal_id": "t", "workspace_id": "w1",
                       "tab_id": "w1:t1", "focused": false, "revision": 1,
                       "agent_status": "blocked", "tokens": { "limit": "5h 68%" } }
        }))]);
        let agent = client(&server).agent_get("w1:p1").unwrap();

        let raw = server.raw.lock().unwrap().clone();
        assert_eq!(raw.len(), 1);
        assert!(raw[0].ends_with('\n') && raw[0].matches('\n').count() == 1);
        let request = &server.requests()[0];
        assert!(request["id"].as_str().is_some_and(|id| !id.is_empty()));
        assert_eq!(request["method"], "agent.get");
        assert_eq!(request["params"], json!({ "target": "w1:p1" }));

        assert_eq!(agent.status, AgentStatus::Blocked);
        assert_eq!(agent.tokens.context, None);
        assert_eq!(agent.tokens.limit.as_deref(), Some("5h 68%"));
    }

    /// A two-pane tree, as herdr 0.9.5 exports it.
    fn tree(ratio: f32) -> Value {
        json!({
            "workspace_id": "w1", "tab_id": "w1:t1", "zoomed": false, "focused_pane_id": "w1:p1",
            "root": { "type": "split", "direction": "right", "ratio": ratio,
                "first": { "type": "pane", "pane_id": "w1:p1", "cwd": "/src" },
                "second": { "type": "pane", "pane_id": "w1:p2", "cwd": "/src" } }
        })
    }

    #[test]
    fn sends_action_params() {
        let server = TestServer::start(vec![
            respond(json!({ "type": "ok" })),
            respond(json!({ "type": "agent_prompted", "agent": {} })),
            respond(json!({ "type": "layout_split_ratio_set", "layout": tree(0.75) })),
            respond(json!({ "type": "ok" })),
            respond(json!({ "type": "plugin_pane_focused", "plugin_pane": {} })),
            respond(json!({ "type": "plugin_pane_closed", "pane_id": "w1:p2" })),
        ]);
        let herdr = client(&server);
        herdr.agent_focus("w1:p1").unwrap();
        herdr.agent_prompt("w1:p1", "hi").unwrap();
        let stored = herdr
            .set_split_ratio("w1:t1", &[false, true], 0.75)
            .unwrap();
        herdr.tab_focus("w1:t1").unwrap();
        herdr.plugin_pane_focus("w1:p2").unwrap();
        herdr.plugin_pane_close("w1:p2").unwrap();
        let params: Vec<Value> = server
            .requests()
            .iter()
            .map(|r| r["params"].clone())
            .collect();
        assert_eq!(
            server.methods(),
            [
                "agent.focus",
                "agent.prompt",
                "layout.set_split_ratio",
                "tab.focus",
                "plugin.pane.focus",
                "plugin.pane.close",
            ]
        );
        assert_eq!(params[1], json!({ "target": "w1:p1", "text": "hi" }));
        assert_eq!(
            params[2],
            json!({ "tab_id": "w1:t1", "path": [false, true], "ratio": 0.75 })
        );
        assert!(
            matches!(stored.root, LayoutNode::Split { ratio, .. } if (ratio - 0.75).abs() < 1e-6)
        );
        assert_eq!(params[3], json!({ "tab_id": "w1:t1" }));
        assert_eq!(params[4], json!({ "pane_id": "w1:p2" }));
        assert_eq!(params[5], json!({ "pane_id": "w1:p2" }));
    }

    #[test]
    fn reads_layouts_and_opens_plugin_panes() {
        let server = TestServer::start(vec![
            respond(json!({ "type": "layout_export", "layout": tree(0.5) })),
            respond(json!({ "type": "pane_layout", "layout": {
                "workspace_id": "w1", "tab_id": "w1:t1", "zoomed": false, "focused_pane_id": "w1:p1",
                "area": { "x": 0, "y": 0, "width": 132, "height": 42 },
                "panes": [
                    { "pane_id": "w1:p1", "focused": true,
                      "rect": { "x": 0, "y": 0, "width": 66, "height": 42 } },
                    { "pane_id": "w1:p2", "focused": false,
                      "rect": { "x": 66, "y": 0, "width": 66, "height": 42 } },
                ],
                "splits": [{ "id": "split_0_root", "direction": "right", "ratio": 0.5,
                             "rect": { "x": 0, "y": 0, "width": 132, "height": 42 } }],
            } })),
            respond(json!({ "type": "plugin_pane_opened", "plugin_pane": {
                "plugin_id": "conductor-deck", "entrypoint": "deck",
                "pane": { "pane_id": "w1:p3", "tab_id": "w1:t1", "workspace_id": "w1",
                          "terminal_id": "t", "focused": false, "agent_status": "unknown" },
            } })),
        ]);
        let herdr = client(&server);

        let tree = herdr.layout_export("w1:t1").unwrap();
        assert_eq!(tree.tab_id, "w1:t1");
        let LayoutNode::Split {
            direction, second, ..
        } = tree.root
        else {
            panic!("expected a split root");
        };
        assert_eq!(direction, SplitDirection::Right);
        assert_eq!(
            *second,
            LayoutNode::Pane {
                pane_id: Some("w1:p2".into())
            }
        );

        let layout = herdr.pane_layout("w1:p2").unwrap();
        assert_eq!(layout.area.width, 132);
        assert_eq!(layout.panes[1].rect.x, 66);

        let opened = herdr
            .plugin_pane_open("conductor-deck", "deck", "w1:p1")
            .unwrap();
        assert_eq!(opened, "w1:p3");

        let params: Vec<Value> = server
            .requests()
            .iter()
            .map(|r| r["params"].clone())
            .collect();
        assert_eq!(
            server.methods(),
            ["layout.export", "pane.layout", "plugin.pane.open"]
        );
        assert_eq!(params[0], json!({ "tab_id": "w1:t1" }));
        assert_eq!(params[1], json!({ "pane_id": "w1:p2" }));
        assert_eq!(
            params[2],
            json!({ "plugin_id": "conductor-deck", "entrypoint": "deck", "placement": "split",
                    "direction": "right", "target_pane_id": "w1:p1" })
        );
    }

    #[test]
    fn maps_error_responses() {
        let server = TestServer::start(vec![
            respond_error("agent_blocked"),
            respond_error("agent_not_found"),
            respond_error("invalid_ratio"),
            respond_raw(vec![
                r#"{"id":"someone-else","result":{"type":"ok"}}"#.to_owned() + "\n",
            ]),
            respond_raw(vec![]),
            respond(json!({ "type": "pong" })),
        ]);
        let herdr = client(&server);
        assert!(matches!(
            herdr.agent_prompt("p", "x"),
            Err(HerdrError::AgentBlocked(_))
        ));
        assert!(matches!(
            herdr.agent_get("p"),
            Err(HerdrError::NotFound { code, .. }) if code == "agent_not_found"
        ));
        assert!(matches!(
            herdr.set_split_ratio("t", &[], 2.0),
            Err(HerdrError::Server { code, .. }) if code == "invalid_ratio"
        ));
        assert!(matches!(
            herdr.agent_focus("p"),
            Err(HerdrError::Protocol(_))
        ));
        assert!(matches!(herdr.snapshot(), Err(HerdrError::Protocol(_))));
        assert!(matches!(herdr.snapshot(), Err(HerdrError::Protocol(_))));
    }

    #[test]
    fn subscribe_reports_unreachable_socket() {
        let herdr = SocketClient::new(socket_path());
        assert!(matches!(herdr.subscribe(&[]), Err(HerdrError::Io(_))));
    }

    #[test]
    fn subscribes_to_global_events_and_watched_panes() {
        let server = TestServer::start(vec![
            subscription(vec![], true),
            respond(snapshot_result(&["w1:p1"])),
        ]);
        let sub = client(&server).subscribe(&["w1:p1".into()]).unwrap();
        assert_eq!(resync_panes(&next(&sub)), ["w1:p1"]);
        assert_eq!(
            server.requests()[0]["params"]["subscriptions"],
            json!([
                { "type": "pane.updated" },
                { "type": "pane.closed" },
                { "type": "layout.updated" },
                { "type": "workspace.closed" },
                { "type": "pane.agent_status_changed", "pane_id": "w1:p1" },
            ])
        );
    }

    #[test]
    fn resyncs_after_events_lost() {
        let server = TestServer::start(vec![
            subscription(vec![pane_updated, events_lost], false),
            respond(snapshot_result(&["w1:p1"])),
            subscription(vec![], true),
            respond(snapshot_result(&["w1:p1", "w1:p2"])),
        ]);
        let sub = client(&server).subscribe(&[]).unwrap();

        assert_eq!(resync_panes(&next(&sub)), ["w1:p1"]);
        assert!(
            matches!(next(&sub), HerdrEvent::PaneUpdated(p) if p.status == AgentStatus::Working)
        );
        assert_eq!(resync_panes(&next(&sub)), ["w1:p1", "w1:p2"]);
        assert_eq!(
            server.methods(),
            [
                "events.subscribe",
                "session.snapshot",
                "events.subscribe",
                "session.snapshot"
            ]
        );
    }

    #[test]
    fn reconnects_after_server_restart() {
        let path = socket_path();
        let server = TestServer::start_at(
            &path,
            vec![
                subscription(vec![], true),
                respond(snapshot_result(&["w1:p1"])),
            ],
        );
        let sub = client(&server).subscribe(&[]).unwrap();
        assert_eq!(resync_panes(&next(&sub)), ["w1:p1"]);

        // The server goes away; the client keeps retrying while nothing is listening.
        drop(server);
        assert_eq!(next(&sub), HerdrEvent::Disconnected);
        std::thread::sleep(Duration::from_millis(30));
        assert!(sub.events.try_recv().is_err());

        let restarted = TestServer::start_at(
            &path,
            vec![
                subscription(vec![], true),
                respond(snapshot_result(&["w1:p2"])),
            ],
        );
        assert_eq!(resync_panes(&next(&sub)), ["w1:p2"]);
        assert_eq!(
            restarted.methods(),
            ["events.subscribe", "session.snapshot"]
        );
    }

    #[test]
    fn reconnects_when_stream_drops() {
        let server = TestServer::start(vec![
            subscription(vec![], true),
            respond(snapshot_result(&[])),
            subscription(vec![], true),
            respond(snapshot_result(&["w1:p1"])),
        ]);
        let sub = client(&server).subscribe(&[]).unwrap();
        assert!(matches!(next(&sub), HerdrEvent::Resync(_)));
        server.drop_connections();
        assert_eq!(next(&sub), HerdrEvent::Disconnected);
        assert_eq!(resync_panes(&next(&sub)), ["w1:p1"]);
    }

    #[test]
    fn prunes_closed_panes_from_subscription() {
        let server = TestServer::start(vec![
            respond_error("pane_not_found"),
            respond(snapshot_result(&["w1:p1"])),
            subscription(vec![], true),
            respond(snapshot_result(&["w1:p1"])),
        ]);
        let sub = client(&server)
            .subscribe(&["w1:p1".into(), "w1:gone".into()])
            .unwrap();
        assert_eq!(resync_panes(&next(&sub)), ["w1:p1"]);

        let watched = |request: &Value| -> Vec<Value> {
            request["params"]["subscriptions"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|s| s.get("pane_id").cloned())
                .collect()
        };
        let requests = server.requests();
        assert_eq!(watched(&requests[0]), [json!("w1:p1"), json!("w1:gone")]);
        assert_eq!(watched(&requests[2]), [json!("w1:p1")]);
    }

    #[test]
    fn dropping_the_stop_handle_ends_the_thread() {
        let server = TestServer::start(vec![
            subscription(vec![], true),
            respond(snapshot_result(&[])),
        ]);
        let Subscription {
            events,
            _stop: stop,
        } = client(&server).subscribe(&[]).unwrap();
        assert!(matches!(
            events.recv_timeout(WAIT),
            Ok(HerdrEvent::Resync(_))
        ));

        drop(stop);
        // The thread drops its sender when it exits, which disconnects the channel.
        let deadline = Instant::now() + WAIT;
        loop {
            match events.recv_timeout(Duration::from_millis(10)) {
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(event) => panic!("unexpected {event:?}"),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    assert!(Instant::now() < deadline, "watcher thread did not stop");
                }
            }
        }
    }
}
