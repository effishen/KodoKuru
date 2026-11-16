//! The KodoKuru agent runtime: tools, the agent loop, and the orchestrator
//! that runs a team of role-based agents per workspace.

pub mod agent;
pub mod context;
pub mod prompt;
pub mod runtime;
pub mod tools;
pub mod workspace;

pub use runtime::{EngineHealth, Runtime};
pub use workspace::Workspace;

use kk_core::Event;
use serde::Serialize;

/// Messages pushed to connected UIs.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "live", rename_all = "snake_case")]
pub enum Live {
    /// A persisted event.
    Event { event: Event },
    /// A streamed fragment of an assistant turn (not persisted).
    Delta {
        agent_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
    },
    /// Progress output from an environment (VM creation, provisioning…).
    EnvLog { name: String, line: String },
    EngineHealth { health: Vec<EngineHealth> },
    Lock { locked: bool },
    WorkspacesChanged,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiveMsg {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(flatten)]
    pub body: Live,
}
