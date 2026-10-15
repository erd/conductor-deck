use serde::Deserialize;

use super::error::Result;
use super::types::{AgentStatus, Layout, Pane, Snapshot};

/// What the deck hears from herdr, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum HerdrEvent {
    AgentStatusChanged {
        pane_id: String,
        workspace_id: String,
        status: AgentStatus,
    },
    PaneUpdated(Pane),
    PaneClosed {
        pane_id: String,
        workspace_id: String,
    },
    LayoutUpdated(Layout),
    WorkspaceClosed {
        workspace_id: String,
    },
    /// Starts every (re)connection, including after `events_lost`: replace all cached state
    /// with this snapshot.
    Resync(Snapshot),
    /// The socket dropped, usually because the herdr server restarted. The client is reconnecting
    /// and sends `Resync` once it is back.
    Disconnected,
}

/// The event types the deck subscribes to for every pane.
pub const GLOBAL_SUBSCRIPTIONS: [&str; 4] = [
    "pane.updated",
    "pane.closed",
    "layout.updated",
    "workspace.closed",
];

/// The subscription type that herdr only accepts per pane.
pub const AGENT_STATUS_SUBSCRIPTION: &str = "pane.agent_status_changed";

#[derive(Deserialize)]
struct Envelope {
    data: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Wire {
    PaneAgentStatusChanged {
        pane_id: String,
        workspace_id: String,
        agent_status: AgentStatus,
    },
    PaneUpdated {
        pane: Pane,
    },
    PaneClosed {
        pane_id: String,
        workspace_id: String,
    },
    LayoutUpdated {
        layout: Layout,
    },
    WorkspaceClosed {
        workspace_id: String,
    },
}

const KNOWN: [&str; 5] = [
    "pane_agent_status_changed",
    "pane_updated",
    "pane_closed",
    "layout_updated",
    "workspace_closed",
];

/// Decodes one event line (`{"event":…,"data":{"type":…,…}}`). Kinds the deck does not use
/// decode to `None`, so a newer herdr can add events without breaking the stream.
pub fn decode_event(line: &str) -> Result<Option<HerdrEvent>> {
    let Envelope { data } = serde_json::from_str(line)?;
    if !data
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| KNOWN.contains(&kind))
    {
        return Ok(None);
    }
    Ok(Some(match serde_json::from_value(data)? {
        Wire::PaneAgentStatusChanged {
            pane_id,
            workspace_id,
            agent_status,
        } => HerdrEvent::AgentStatusChanged {
            pane_id,
            workspace_id,
            status: agent_status,
        },
        Wire::PaneUpdated { pane } => HerdrEvent::PaneUpdated(pane),
        Wire::PaneClosed {
            pane_id,
            workspace_id,
        } => HerdrEvent::PaneClosed {
            pane_id,
            workspace_id,
        },
        Wire::LayoutUpdated { layout } => HerdrEvent::LayoutUpdated(layout),
        Wire::WorkspaceClosed { workspace_id } => HerdrEvent::WorkspaceClosed { workspace_id },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::types::{Rect, SplitDirection};

    // Shapes captured from herdr 0.9.5.
    const PANE_UPDATED: &str = r#"{"event":"pane_updated","data":{"type":"pane_updated","pane":{"agent":"claude","agent_session":{"agent":"claude","kind":"id","source":"herdr:claude","value":"6af3"},"agent_status":"done","cwd":"/tmp","focused":true,"pane_id":"w61:p1","revision":4,"scroll":{"max_offset_from_bottom":0,"offset_from_bottom":0,"viewport_rows":64},"state_labels":null,"tab_id":"w61:t1","terminal_id":"term_6","tokens":{"context":"⛁ 14% (138k)","limit":"5h 68%","provider":"claude","title":"Claude"},"workspace_id":"w61"}}}"#;

    #[test]
    fn decodes_pane_updated() {
        let Some(HerdrEvent::PaneUpdated(pane)) = decode_event(PANE_UPDATED).unwrap() else {
            panic!("expected PaneUpdated");
        };
        assert_eq!(pane.pane_id, "w61:p1");
        assert_eq!(pane.status, AgentStatus::Done);
        assert_eq!(pane.tokens.context.as_deref(), Some("⛁ 14% (138k)"));
    }

    #[test]
    fn decodes_agent_status_changed() {
        let line = r#"{"event":"pane_agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"w1:p2","workspace_id":"w1","agent_status":"blocked","agent":"claude","state_labels":null}}"#;
        assert_eq!(
            decode_event(line).unwrap(),
            Some(HerdrEvent::AgentStatusChanged {
                pane_id: "w1:p2".into(),
                workspace_id: "w1".into(),
                status: AgentStatus::Blocked,
            })
        );
    }

    #[test]
    fn decodes_pane_and_workspace_closed() {
        let line = r#"{"event":"pane_closed","data":{"type":"pane_closed","pane_id":"w1:p2","workspace_id":"w1"}}"#;
        assert_eq!(
            decode_event(line).unwrap(),
            Some(HerdrEvent::PaneClosed {
                pane_id: "w1:p2".into(),
                workspace_id: "w1".into(),
            })
        );
        let line = r#"{"event":"workspace_closed","data":{"type":"workspace_closed","workspace_id":"w1","workspace":null}}"#;
        assert_eq!(
            decode_event(line).unwrap(),
            Some(HerdrEvent::WorkspaceClosed {
                workspace_id: "w1".into()
            })
        );
    }

    #[test]
    fn decodes_layout_updated() {
        let line = r#"{"event":"layout_updated","data":{"type":"layout_updated","layout":{"area":{"height":64,"width":145,"x":0,"y":0},"focused_pane_id":"wF:p1","panes":[{"focused":true,"pane_id":"wF:p1","rect":{"height":64,"width":100,"x":0,"y":0}},{"focused":false,"pane_id":"wF:p2","rect":{"height":64,"width":44,"x":101,"y":0}}],"splits":[{"id":"s1","direction":"right","ratio":0.7,"rect":{"height":64,"width":145,"x":0,"y":0}}],"tab_id":"wF:t3","workspace_id":"wF","zoomed":false}}}"#;
        let Some(HerdrEvent::LayoutUpdated(layout)) = decode_event(line).unwrap() else {
            panic!("expected LayoutUpdated");
        };
        assert_eq!(layout.tab_id, "wF:t3");
        assert_eq!(layout.panes.len(), 2);
        assert_eq!(layout.splits[0].direction, SplitDirection::Right);
        assert_eq!(
            layout.area,
            Rect {
                x: 0,
                y: 0,
                width: 145,
                height: 64
            }
        );
    }

    #[test]
    fn skips_unknown_kinds() {
        let line =
            r#"{"event":"tab_renamed","data":{"type":"tab_renamed","tab_id":"w1:t1","label":"x"}}"#;
        assert_eq!(decode_event(line).unwrap(), None);
    }

    #[test]
    fn rejects_malformed_known_kinds() {
        let line = r#"{"event":"pane_closed","data":{"type":"pane_closed"}}"#;
        assert!(decode_event(line).is_err());
        assert!(decode_event("not json").is_err());
    }
}
