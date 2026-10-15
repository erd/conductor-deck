//! A scripted stand-in for the herdr socket. Each accepted connection gets the next handler,
//! which sees the parsed request line and writes whatever the test wants back.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Value, json};

/// Returns the stream to hold it open until the server is dropped, or `None` to close it.
pub type Handler = Box<dyn FnOnce(&Value, UnixStream) -> Option<UnixStream> + Send>;

pub struct TestServer {
    pub path: PathBuf,
    /// Every request line received, in order, including the trailing newline.
    pub raw: Arc<Mutex<Vec<String>>>,
    kept: Arc<Mutex<Vec<UnixStream>>>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// A short, unique socket path: macOS caps socket paths at 104 bytes.
pub fn socket_path() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "cdeck-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("h.sock")
}

impl TestServer {
    pub fn start(handlers: Vec<Handler>) -> Self {
        Self::start_at(&socket_path(), handlers)
    }

    pub fn start_at(path: &Path, handlers: Vec<Handler>) -> Self {
        let listener = UnixListener::bind(path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let raw = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread = {
            let (raw, kept, shutdown) = (raw.clone(), kept.clone(), shutdown.clone());
            let mut handlers = VecDeque::from(handlers);
            std::thread::spawn(move || {
                while !shutdown.load(Ordering::Relaxed) {
                    let stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(err) if err.kind() == ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                            continue;
                        }
                        Err(err) => panic!("accept: {err}"),
                    };
                    stream.set_nonblocking(false).unwrap();
                    let mut line = String::new();
                    BufReader::new(&stream).read_line(&mut line).unwrap();
                    raw.lock().unwrap().push(line.clone());
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let handler = handlers
                        .pop_front()
                        .unwrap_or_else(|| panic!("unexpected request {line}"));
                    if let Some(stream) = handler(&request, stream) {
                        kept.lock().unwrap().push(stream);
                    }
                }
            })
        };
        Self {
            path: path.to_owned(),
            raw,
            kept,
            shutdown,
            thread: Some(thread),
        }
    }

    /// The parsed requests received so far.
    pub fn requests(&self) -> Vec<Value> {
        self.raw
            .lock()
            .unwrap()
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    pub fn methods(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|r| r["method"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Closes every held connection, as a restarting herdr server would.
    pub fn drop_connections(&self) {
        for stream in self.kept.lock().unwrap().drain(..) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.drop_connections();
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn result_line(request: &Value, result: &Value) -> String {
    format!("{}\n", json!({ "id": request["id"], "result": result }))
}

pub fn error_line(request: &Value, code: &str) -> String {
    format!(
        "{}\n",
        json!({ "id": request["id"], "error": { "code": code, "message": format!("{code} message") } })
    )
}

/// Answers with `result` and closes, as herdr does for every non-subscription request.
pub fn respond(result: Value) -> Handler {
    Box::new(move |request, mut stream| {
        stream
            .write_all(result_line(request, &result).as_bytes())
            .unwrap();
        None
    })
}

pub fn respond_error(code: &'static str) -> Handler {
    Box::new(move |request, mut stream| {
        stream
            .write_all(error_line(request, code).as_bytes())
            .unwrap();
        None
    })
}

/// Writes raw lines and closes.
pub fn respond_raw(lines: Vec<String>) -> Handler {
    Box::new(move |_, mut stream| {
        stream.write_all(lines.concat().as_bytes()).unwrap();
        None
    })
}

/// Acknowledges a subscription, writes `then` (each built from the request), and either holds
/// the stream open or closes it.
pub fn subscription(then: Vec<fn(&Value) -> String>, hold_open: bool) -> Handler {
    Box::new(move |request, mut stream| {
        stream
            .write_all(result_line(request, &json!({ "type": "subscription_started" })).as_bytes())
            .unwrap();
        for line in then {
            stream.write_all(line(request).as_bytes()).unwrap();
        }
        hold_open.then_some(stream)
    })
}

/// A `session.snapshot` result holding the given pane ids, all in workspace `w1`.
pub fn snapshot_result(panes: &[&str]) -> Value {
    let panes: Vec<Value> = panes
        .iter()
        .map(|id| {
            json!({ "pane_id": id, "terminal_id": "t", "workspace_id": "w1", "tab_id": "w1:t1",
                    "focused": false, "agent_status": "idle", "revision": 0 })
        })
        .collect();
    json!({
        "type": "session_snapshot",
        "snapshot": { "version": "0.9.5", "protocol": 22, "workspaces": [], "tabs": [],
                      "panes": panes, "layouts": [], "agents": [] }
    })
}
