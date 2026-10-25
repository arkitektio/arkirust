//! Why an agent stopped, whichever socket it runs over.

#[allow(unused_imports)]
use crate::prelude::*;
use rekuest_session::SessionError;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("could not get a token: {0}")]
    Token(#[from] fakts::FaktsError),
    #[error("the server refused the registration: {0}")]
    Refused(String),
    #[error("the agent was kicked: {}", .0.as_deref().unwrap_or("no reason given"))]
    Kicked(Option<String>),
    #[error("the server closed the connection with code {code}: {reason}")]
    Closed { code: u16, reason: String },
    #[error("giving up after {0} failed connection attempts")]
    Exhausted(usize),
    #[error("the agent could not start: {0}")]
    Activation(String),
}

impl From<SessionError> for AgentError {
    fn from(error: SessionError) -> Self {
        match error {
            SessionError::Refused(e) => Self::Refused(e),
            SessionError::Kicked(reason) => Self::Kicked(reason),
            SessionError::Closed { code, reason } => Self::Closed { code, reason },
            SessionError::Exhausted(n) => Self::Exhausted(n),
            SessionError::Activation(e) => Self::Activation(e),
        }
    }
}
