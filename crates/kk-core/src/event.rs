//! The append-only event log. Everything that happens in a workspace is an
//! event; the UI replays events and agent conversations are rebuilt from them.

use serde::{Deserialize, Serialize};

use crate::{ChecklistItem, Role, Task};

/// Who produced something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Sender {
    User,
    Agent { id: String },
    /// KodoKuru itself (nudges, completion checks, interruption notices).
    Harness,
}

impl Sender {
    pub fn agent(id: impl Into<String>) -> Self {
        Sender::Agent { id: id.into() }
    }
    pub fn agent_id(&self) -> Option<&str> {
        match self {
            Sender::Agent { id } => Some(id),
            _ => None,
        }
    }
}

/// Whether the event is part of the user-facing conversation or internal
/// team activity. The user can always see internal events in the UI; the
/// flag lets views (e.g. a game "table") hide them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    #[default]
    Public,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON arguments as produced by the model.
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    #[serde(default)]
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub role_id: String,
    pub is_lead: bool,
    pub created_by: Sender,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageKind {
    Chat,
    TaskAssignment { task_id: String },
    Report,
    /// Harness control text (nudges, completion checks).
    Harness,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunState {
    Idle,
    Running,
    /// Engine offline or busy; the run will continue when it is back.
    WaitingEngine { engine: String, reason: String },
    WaitingApproval,
    /// Waiting for the vault to be unlocked.
    Locked,
    Stopped,
    Paused { reason: String },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    WorkspaceCreated {
        name: String,
        template: String,
    },
    AgentCreated {
        agent: AgentInfo,
    },
    RoleDefined {
        role: Role,
        by: Sender,
    },
    /// Input to an agent: from the user, another agent, or the harness.
    Message {
        to: String,
        from: Sender,
        text: String,
        kind: MessageKind,
    },
    /// A complete assistant turn (streamed deltas are not persisted).
    Assistant {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
        #[serde(default)]
        tool_calls: Vec<ToolCall>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        engine: String,
    },
    ToolResult {
        call_id: String,
        name: String,
        output: String,
        is_error: bool,
    },
    Checklist {
        items: Vec<ChecklistItem>,
    },
    TaskUpsert {
        task: Task,
    },
    RunState {
        state: RunState,
    },
    ApprovalRequested {
        approval_id: String,
        call_id: String,
        tool: String,
        summary: String,
        arguments: String,
    },
    ApprovalResolved {
        approval_id: String,
        approved: bool,
        #[serde(default)]
        note: String,
    },
    /// Older history of an agent summarised to fit the context window.
    Compaction {
        summary: String,
        /// Events with seq <= this are replaced by the summary.
        upto_seq: i64,
    },
    StateSet {
        key: String,
        value: serde_json::Value,
    },
    EnvState {
        name: String,
        state: String,
        #[serde(default)]
        detail: String,
    },
    Notice {
        level: String,
        text: String,
    },
}

/// A persisted event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: i64,
    pub ts: crate::Timestamp,
    /// The agent whose timeline this belongs to, if any.
    pub agent_id: Option<String>,
    pub visibility: Visibility,
    #[serde(flatten)]
    pub kind: EventKind,
}

/// An event before it has been assigned a sequence number.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub agent_id: Option<String>,
    pub visibility: Visibility,
    pub kind: EventKind,
}

impl NewEvent {
    pub fn new(agent_id: Option<&str>, kind: EventKind) -> Self {
        Self { agent_id: agent_id.map(str::to_string), visibility: Visibility::Public, kind }
    }
    pub fn internal(mut self) -> Self {
        self.visibility = Visibility::Internal;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_json_shape() {
        let e = Event {
            seq: 1,
            ts: crate::now(),
            agent_id: Some("a-1".into()),
            visibility: Visibility::Public,
            kind: EventKind::Message {
                to: "a-1".into(),
                from: Sender::User,
                text: "hi".into(),
                kind: MessageKind::Chat,
            },
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["from"]["kind"], "user");
        let back: Event = serde_json::from_value(v).unwrap();
        assert_eq!(back.kind, e.kind);
    }
}
