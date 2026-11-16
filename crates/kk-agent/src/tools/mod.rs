//! Tools agents can call. A role lists tool groups (`fs`, `shell`, …) or
//! individual tool names; the registry filters by that list.

mod board;
mod envs;
mod fs;
mod media;
mod shell;
mod state;
mod team;

use anyhow::Result;
use async_trait::async_trait;
use kk_core::{AgentInfo, EngineKind, Role, ToolCall};
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::workspace::Workspace;

pub struct ToolCtx {
    pub ws: Arc<Workspace>,
    pub agent: AgentInfo,
    pub role: Role,
    pub cancel: CancellationToken,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn group(&self) -> &'static str;
    fn description(&self) -> String;
    fn parameters(&self) -> Value;
    /// Some(summary) if this call needs the user's approval first.
    async fn approval(&self, _ctx: &ToolCtx, _args: &Value) -> Option<String> {
        None
    }
    /// Whether the tool is usable by this role at all (e.g. media tools need
    /// a matching engine).
    async fn available(&self, _ctx: &ToolCtx) -> bool {
        true
    }
    async fn run(&self, ctx: &ToolCtx, args: Value) -> Result<String>;
}

pub fn all() -> Vec<Arc<dyn Tool>> {
    let mut v: Vec<Arc<dyn Tool>> = Vec::new();
    v.extend(fs::tools());
    v.extend(shell::tools());
    v.extend(board::tools());
    v.extend(team::tools());
    v.extend(envs::tools());
    v.extend(state::tools());
    v.extend(media::tools());
    v
}

pub const GROUPS: &[(&str, &str)] = &[
    ("fs", "Read, write, edit, search, move and delete workspace files"),
    ("shell", "Run shell commands in an environment"),
    ("checklist", "Maintain a personal step-by-step checklist"),
    ("board", "Read and update the shared task board"),
    ("team", "Define roles, spawn agents, assign tasks, message agents"),
    ("env", "List, define, start and stop environments"),
    ("state", "Read/write the structured world state and roll dice"),
    ("media", "Text-to-speech and image generation through capability engines"),
];

fn role_allows(role: &Role, tool: &dyn Tool) -> bool {
    // Everyone gets the checklist: it is how KodoKuru keeps agents on track.
    if tool.group() == "checklist" {
        return true;
    }
    role.tools.iter().any(|t| t == "*" || t == tool.group() || t == tool.name())
}

/// Tools available to an agent with this role.
pub async fn for_role(ctx: &ToolCtx) -> Vec<Arc<dyn Tool>> {
    let mut out = Vec::new();
    for t in all() {
        if role_allows(&ctx.role, t.as_ref()) && t.available(ctx).await {
            out.push(t);
        }
    }
    out
}

pub fn specs(tools: &[Arc<dyn Tool>]) -> Vec<kk_llm::ToolSpec> {
    tools
        .iter()
        .map(|t| kk_llm::ToolSpec { name: t.name().into(), description: t.description(), parameters: t.parameters() })
        .collect()
}

/// Tool descriptions for engines in text tool-call mode.
pub fn text_instructions(tools: &[Arc<dyn Tool>]) -> String {
    let mut s = String::from(
        "To call a tool, write a block exactly like this (one block per call, JSON inside):\n<tool_call>\n{\"name\": \"tool_name\", \"arguments\": {\"param\": \"value\"}}\n</tool_call>\nYou may call several tools in one reply. Results come back as `[Tool result: name]` messages. When you need no tool, just answer.\n\nAvailable tools:\n",
    );
    for t in tools {
        s.push_str(&format!("\n### {}\n{}\nParameters (JSON schema): {}\n", t.name(), t.description(), t.parameters()));
    }
    s
}

/// Parse tool arguments, repairing slightly broken JSON from local models.
pub fn parse_args(call: &ToolCall) -> Result<Value, String> {
    let fixed = kk_llm::repair_json(&call.arguments).ok_or_else(|| {
        format!(
            "Could not parse the arguments of `{}` as JSON: {}. Send the call again with valid JSON arguments.",
            call.name,
            kk_llm::truncate(&call.arguments, 300)
        )
    })?;
    let v: Value = serde_json::from_str(&fixed).map_err(|e| e.to_string())?;
    match v {
        Value::Object(_) => Ok(v),
        Value::Null => Ok(Value::Object(Default::default())),
        other => Err(format!("arguments of `{}` must be a JSON object, got {other}", call.name)),
    }
}

// --------------------------------------------------------------- helpers

pub(crate) fn s<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required string parameter `{key}`"))
}

pub(crate) fn opt_s<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

pub(crate) fn opt_u(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
}

pub(crate) fn opt_b(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true")))
}

pub(crate) fn str_list(args: &Value, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        Some(Value::String(s)) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        _ => vec![],
    }
}

/// Capability engines of a kind this role may use.
pub(crate) async fn role_engines(ctx: &ToolCtx, kind: EngineKind) -> Vec<kk_core::EngineConfig> {
    let engines = ctx.ws.rt.engines().await;
    let settings = ctx.ws.settings();
    engines
        .engines
        .into_iter()
        .filter(|e| e.kind == kind && ctx.role.engines.iter().any(|r| r == &e.id) && settings.engine_allowed(&e.id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_unique_and_grouped() {
        let tools = all();
        let mut names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        names.sort();
        let n = names.len();
        names.dedup();
        assert_eq!(n, names.len(), "duplicate tool names");
        for t in &tools {
            assert!(GROUPS.iter().any(|(g, _)| *g == t.group()), "{} has unknown group", t.name());
            assert_eq!(t.parameters()["type"], "object", "{}", t.name());
        }
    }

    #[test]
    fn args_parsing() {
        let c = ToolCall { id: "1".into(), name: "x".into(), arguments: "{\"a\": 1,}".into() };
        assert_eq!(parse_args(&c).unwrap()["a"], 1);
        let c = ToolCall { id: "1".into(), name: "x".into(), arguments: "".into() };
        assert!(parse_args(&c).unwrap().is_object());
        let c = ToolCall { id: "1".into(), name: "x".into(), arguments: "[1]".into() };
        assert!(parse_args(&c).is_err());
    }
}
