// Field names mirror herdr's wire names, so `Pane::pane_id` stays `pane_id`.
#![expect(clippy::struct_field_names, reason = "fields mirror herdr's JSON")]

use std::collections::HashMap;

use serde::{Deserialize, Deserializer};

use super::error::{HerdrError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    #[serde(other)]
    Unknown,
}

impl AgentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Unknown => "unknown",
        }
    }
}

/// The agent's token readouts, as herdr formats them (e.g. `⛁ 26% (263k)` and `5h 68%`).
///
/// herdr sends a map of display strings; only `context` and `limit` are kept. Either can be
/// missing, and herdr does not restore them after a server restart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tokens {
    pub context: Option<String>,
    pub limit: Option<String>,
}

impl<'de> Deserialize<'de> for Tokens {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let mut map =
            Option::<HashMap<String, String>>::deserialize(deserializer)?.unwrap_or_default();
        Ok(Self {
            context: map.remove("context"),
            limit: map.remove("limit"),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Agent {
    pub pane_id: String,
    pub tab_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(rename = "agent_status")]
    pub status: AgentStatus,
    #[serde(default)]
    pub tokens: Tokens,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Pane {
    pub pane_id: String,
    pub tab_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(rename = "agent_status")]
    pub status: AgentStatus,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub tokens: Tokens,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Workspace {
    pub workspace_id: String,
    pub label: String,
    #[serde(default)]
    pub focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Tab {
    pub tab_id: String,
    pub workspace_id: String,
    pub label: String,
    #[serde(default)]
    pub focused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LayoutPane {
    pub pane_id: String,
    pub rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Right,
    Down,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LayoutSplit {
    pub id: String,
    pub direction: SplitDirection,
    pub ratio: f32,
    pub rect: Rect,
}

/// One tab's pane geometry.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Layout {
    pub workspace_id: String,
    pub tab_id: String,
    #[serde(default)]
    pub zoomed: bool,
    pub area: Rect,
    pub panes: Vec<LayoutPane>,
    pub splits: Vec<LayoutSplit>,
}

/// herdr's whole live session, from `session.snapshot`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
    #[serde(default)]
    pub focused_tab_id: Option<String>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
    pub workspaces: Vec<Workspace>,
    pub tabs: Vec<Tab>,
    pub panes: Vec<Pane>,
    pub layouts: Vec<Layout>,
    pub agents: Vec<Agent>,
}

/// Where the deck itself is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginContext {
    pub pane_id: String,
    pub tab_id: Option<String>,
    pub workspace_id: Option<String>,
    pub workspace_label: Option<String>,
    pub tab_label: Option<String>,
    pub focused_pane_id: Option<String>,
}

/// The fields the deck reads from `HERDR_PLUGIN_CONTEXT_JSON`; herdr makes every one nullable.
#[derive(Debug, Default, Deserialize)]
struct RawContext {
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_label: Option<String>,
    #[serde(default)]
    tab_id: Option<String>,
    #[serde(default)]
    tab_label: Option<String>,
    #[serde(default)]
    focused_pane_id: Option<String>,
}

impl PluginContext {
    /// Reads the context herdr gives plugin commands.
    pub fn from_env() -> Result<Self> {
        let var = |name| std::env::var(name).ok().filter(|v: &String| !v.is_empty());
        Self::from_parts(
            var("HERDR_PLUGIN_CONTEXT_JSON").as_deref(),
            var("HERDR_PANE_ID"),
            var("HERDR_TAB_ID"),
            var("HERDR_WORKSPACE_ID"),
        )
    }

    /// The context JSON carries no pane id, so the deck's own pane always comes from
    /// `HERDR_PANE_ID`; tab and workspace prefer the JSON and fall back to their env vars.
    pub fn from_parts(
        json: Option<&str>,
        pane_id: Option<String>,
        tab_id: Option<String>,
        workspace_id: Option<String>,
    ) -> Result<Self> {
        let raw: RawContext = match json {
            Some(json) => serde_json::from_str::<Option<RawContext>>(json)
                .map_err(|err| HerdrError::protocol(format!("HERDR_PLUGIN_CONTEXT_JSON: {err}")))?
                .unwrap_or_default(),
            None => RawContext::default(),
        };
        Ok(Self {
            pane_id: pane_id.ok_or(HerdrError::MissingEnv("HERDR_PANE_ID"))?,
            tab_id: raw.tab_id.or(tab_id),
            workspace_id: raw.workspace_id.or(workspace_id),
            workspace_label: raw.workspace_label,
            tab_label: raw.tab_label,
            focused_pane_id: raw.focused_pane_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[expect(
        clippy::unnecessary_wraps,
        reason = "builds the Option arguments under test"
    )]
    fn some(s: &str) -> Option<String> {
        Some(s.to_owned())
    }

    #[test]
    fn decodes_agent_with_tokens() {
        let agent: Agent = serde_json::from_str(
            r#"{"terminal_id":"term_1","agent":"claude","agent_status":"working",
                "tokens":{"context":"⛁ 26% (263k)","limit":"5h 68%","provider":"claude"},
                "workspace_id":"w1","tab_id":"w1:t1","pane_id":"w1:p1","focused":false,
                "state_labels":null,"revision":3,"some_future_field":1}"#,
        )
        .unwrap();
        assert_eq!(agent.pane_id, "w1:p1");
        assert_eq!(agent.status, AgentStatus::Working);
        assert_eq!(agent.tokens.context.as_deref(), Some("⛁ 26% (263k)"));
        assert_eq!(agent.tokens.limit.as_deref(), Some("5h 68%"));
    }

    #[test]
    fn missing_tokens_and_unknown_status_are_tolerated() {
        let agent: Agent = serde_json::from_str(
            r#"{"agent_status":"thinking","workspace_id":"w1","tab_id":"w1:t1","pane_id":"w1:p1"}"#,
        )
        .unwrap();
        assert_eq!(agent.status, AgentStatus::Unknown);
        assert_eq!(agent.tokens, Tokens::default());

        let agent: Agent = serde_json::from_str(
            r#"{"agent_status":"idle","workspace_id":"w1","tab_id":"w1:t1","pane_id":"w1:p1",
                "tokens":{"limit":"5h 68%"}}"#,
        )
        .unwrap();
        assert_eq!(agent.tokens.context, None);
        assert_eq!(agent.tokens.limit.as_deref(), Some("5h 68%"));
    }

    #[test]
    fn context_prefers_json_ids_and_takes_pane_from_env() {
        let ctx = PluginContext::from_parts(
            Some(r#"{"workspace_id":"w2","tab_id":"w2:t1","workspace_label":"api","selected_text":null}"#),
            some("w2:p3"),
            some("env:t"),
            some("env"),
        )
        .unwrap();
        assert_eq!(ctx.pane_id, "w2:p3");
        assert_eq!(ctx.tab_id, some("w2:t1"));
        assert_eq!(ctx.workspace_id, some("w2"));
        assert_eq!(ctx.workspace_label, some("api"));
    }

    #[test]
    fn context_falls_back_to_env() {
        for json in [None, Some("null"), Some(r#"{"workspace_id":null}"#)] {
            let ctx =
                PluginContext::from_parts(json, some("w1:p1"), some("w1:t1"), some("w1")).unwrap();
            assert_eq!(ctx.tab_id, some("w1:t1"), "{json:?}");
            assert_eq!(ctx.workspace_id, some("w1"), "{json:?}");
        }
    }

    #[test]
    fn context_requires_pane_id() {
        let err = PluginContext::from_parts(None, None, None, None).unwrap_err();
        assert!(matches!(err, HerdrError::MissingEnv("HERDR_PANE_ID")));
        let err = PluginContext::from_parts(Some("{"), some("p"), None, None).unwrap_err();
        assert!(matches!(err, HerdrError::Protocol(_)));
    }
}
