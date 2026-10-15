use std::io;

pub type Result<T> = std::result::Result<T, HerdrError>;

#[derive(Debug, thiserror::Error)]
pub enum HerdrError {
    #[error("{0} is not set; the deck must run inside a herdr plugin pane")]
    MissingEnv(&'static str),
    #[error("herdr socket: {0}")]
    Io(#[from] io::Error),
    #[error("herdr protocol: {0}")]
    Protocol(String),
    /// herdr refuses to prompt an agent that is waiting on its own question.
    #[error("agent is blocked: {0}")]
    AgentBlocked(String),
    #[error("{code}: {message}")]
    NotFound { code: String, message: String },
    /// The subscription fell behind herdr's retained history; cached state is stale.
    #[error("event subscription fell behind")]
    EventsLost,
    #[error("herdr error {code}: {message}")]
    Server { code: String, message: String },
}

impl HerdrError {
    /// Maps an error body from the socket to a typed error.
    pub fn from_code(code: String, message: String) -> Self {
        match code.as_str() {
            "agent_blocked" => Self::AgentBlocked(message),
            "events_lost" => Self::EventsLost,
            "not_found" | "agent_not_found" | "pane_not_found" | "split_not_found" => {
                Self::NotFound { code, message }
            }
            _ => Self::Server { code, message },
        }
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol(message.into())
    }
}

impl From<serde_json::Error> for HerdrError {
    fn from(err: serde_json::Error) -> Self {
        Self::Protocol(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(code: &str) -> HerdrError {
        HerdrError::from_code(code.into(), "msg".into())
    }

    #[test]
    fn maps_codes_to_variants() {
        assert!(matches!(map("agent_blocked"), HerdrError::AgentBlocked(m) if m == "msg"));
        assert!(matches!(map("events_lost"), HerdrError::EventsLost));
        for code in [
            "not_found",
            "agent_not_found",
            "pane_not_found",
            "split_not_found",
        ] {
            assert!(matches!(map(code), HerdrError::NotFound { .. }), "{code}");
        }
        assert!(
            matches!(map("invalid_ratio"), HerdrError::Server { code, .. } if code == "invalid_ratio")
        );
    }
}
