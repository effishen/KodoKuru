//! Team tools: engines and roles catalog, defining roles, spawning agents,
//! assigning tasks and messaging.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use kk_core::*;
use serde_json::{json, Value};
use std::sync::Arc;

use super::{opt_s, opt_u, s, str_list, Tool, ToolCtx};

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ListEngines),
        Arc::new(ListRoles),
        Arc::new(DefineRole),
        Arc::new(ListAgents),
        Arc::new(SpawnAgent),
        Arc::new(AssignTask),
        Arc::new(MessageAgent),
        Arc::new(ReadTranscript),
    ]
}

fn team_approval(ctx: &ToolCtx, summary: String) -> Option<String> {
    (ctx.ws.settings().team_policy == Policy::Ask).then_some(summary)
}

struct ListEngines;
#[async_trait]
impl Tool for ListEngines {
    fn name(&self) -> &'static str {
        "list_engines"
    }
    fn group(&self) -> &'static str {
        "team"
    }
    fn description(&self) -> String {
        "List the engines (chat models, text-to-speech, image…) you can give to roles, with notes, cost, context size and whether they are online.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    async fn run(&self, ctx: &ToolCtx, _a: Value) -> Result<String> {
        let engines = ctx.ws.rt.engines().await;
        let health = ctx.ws.rt.health().await;
        let settings = ctx.ws.settings();
        if engines.engines.is_empty() {
            return Ok("No engines are configured.".into());
        }
        let mut out = Vec::new();
        for e in &engines.engines {
            let h = health.iter().find(|h| h.id == e.id).map(|h| h.status.as_str()).unwrap_or("unknown");
            let allowed = if settings.engine_allowed(&e.id) { "" } else { " [NOT allowed in this workspace]" };
            let default = if engines.default_chat == e.id { " (default chat)" } else { "" };
            out.push(format!(
                "- {}{default}: kind={} model={} cost={} status={h}{}{allowed}\n  {}",
                e.id,
                e.kind.as_str(),
                if e.model.is_empty() { "-" } else { &e.model },
                e.cost,
                if e.kind == EngineKind::Chat { format!(" context={}", e.context_window) } else { String::new() },
                if e.notes.is_empty() { "(no notes)" } else { &e.notes }
            ));
        }
        out.push("\nChat engines are an agent's brain (role.engine). Other kinds are tools: list them in role.engines and give the role the `media` tool group.".into());
        Ok(out.join("\n"))
    }
}

struct ListRoles;
#[async_trait]
impl Tool for ListRoles {
    fn name(&self) -> &'static str {
        "list_roles"
    }
    fn group(&self) -> &'static str {
        "team"
    }
    fn description(&self) -> String {
        "List roles defined in this workspace and in the user's role library.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    async fn run(&self, ctx: &ToolCtx, _a: Value) -> Result<String> {
        let local = ctx.ws.roles()?;
        let mut out = vec!["Workspace roles:".to_string()];
        for r in &local {
            out.push(format!(
                "- {} ({}): {} | engine={} tools={} engines={}",
                r.id,
                r.name,
                r.description,
                r.engine.as_deref().unwrap_or("default"),
                r.tools.join(","),
                if r.engines.is_empty() { "-".into() } else { r.engines.join(",") }
            ));
        }
        let lib: Vec<_> = ctx.ws.rt.home.roles()?.into_iter().filter(|(r, _)| !local.iter().any(|l| l.id == r.id)).collect();
        if !lib.is_empty() {
            out.push("\nLibrary roles (copied into the workspace when used):".into());
            for (r, _) in lib {
                out.push(format!("- {} ({}): {}", r.id, r.name, r.description));
            }
        }
        out.push(format!(
            "\nTool groups: {}",
            super::GROUPS.iter().map(|(g, d)| format!("{g} ({d})")).collect::<Vec<_>>().join("; ")
        ));
        Ok(out.join("\n"))
    }
}

struct DefineRole;
#[async_trait]
impl Tool for DefineRole {
    fn name(&self) -> &'static str {
        "define_role"
    }
    fn group(&self) -> &'static str {
        "team"
    }
    fn description(&self) -> String {
        "Create or replace a role in this workspace. Choose a chat engine for its reasoning, the tool groups it needs, and any capability engines (tts, image) it may use.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["id", "name", "description", "prompt", "tools"], "properties": {
            "id": {"type": "string", "description": "lowercase identifier, e.g. sound-designer"},
            "name": {"type": "string"},
            "description": {"type": "string", "description": "one line: what this role is for"},
            "prompt": {"type": "string", "description": "role instructions: responsibilities, standards, what to report"},
            "engine": {"type": "string", "description": "chat engine id (omit for the default)"},
            "tools": {"type": "array", "items": {"type": "string"}, "description": "tool groups or tool names"},
            "engines": {"type": "array", "items": {"type": "string"}, "description": "capability engine ids (tts/image…)"},
            "environment": {"type": "string"}
        }})
    }
    async fn approval(&self, ctx: &ToolCtx, a: &Value) -> Option<String> {
        team_approval(
            ctx,
            format!(
                "Define role `{}` (engine {}, tools {})",
                a.get("id").and_then(|v| v.as_str()).unwrap_or("?"),
                a.get("engine").and_then(|v| v.as_str()).unwrap_or("default"),
                str_list(a, "tools").join(",")
            ),
        )
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let engines = ctx.ws.rt.engines().await;
        let settings = ctx.ws.settings();
        let engine = opt_s(&a, "engine").map(str::to_string);
        if let Some(e) = &engine {
            let cfg = engines.get(e).with_context(|| format!("no engine {e:?}; see list_engines"))?;
            if cfg.kind != EngineKind::Chat {
                bail!("engine {e:?} is a {} engine; put it in `engines`, not `engine`", cfg.kind.as_str());
            }
            if !settings.engine_allowed(e) {
                bail!("engine {e:?} is not allowed in this workspace");
            }
        }
        let caps = str_list(&a, "engines");
        for e in &caps {
            let cfg = engines.get(e).with_context(|| format!("no engine {e:?}; see list_engines"))?;
            if !settings.engine_allowed(e) {
                bail!("engine {:?} is not allowed in this workspace", cfg.id);
            }
        }
        let mut tools = str_list(&a, "tools");
        if !caps.is_empty() && !tools.iter().any(|t| t == "media") {
            tools.push("media".into());
        }
        let role = Role {
            id: s(&a, "id")?.trim().to_lowercase(),
            name: s(&a, "name")?.trim().to_string(),
            description: opt_s(&a, "description").unwrap_or("").to_string(),
            prompt: s(&a, "prompt")?.to_string(),
            engine,
            tools,
            engines: caps,
            environment: opt_s(&a, "environment").map(str::to_string),
            temperature: None,
            reasoning_effort: None,
            self_check: true,
        };
        role.validate()?;
        ctx.ws.save_role(&role, Sender::agent(&ctx.agent.id))?;
        Ok(format!("Role `{}` saved. Spawn an agent with spawn_agent(role=\"{}\").", role.id, role.id))
    }
}

struct ListAgents;
#[async_trait]
impl Tool for ListAgents {
    fn name(&self) -> &'static str {
        "list_agents"
    }
    fn group(&self) -> &'static str {
        "team"
    }
    fn description(&self) -> String {
        "List the agents in this workspace with their role, status and open tasks.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    async fn run(&self, ctx: &ToolCtx, _a: Value) -> Result<String> {
        let tasks = ctx.ws.store().tasks()?;
        let mut out = Vec::new();
        for a in ctx.ws.agents()? {
            let st = ctx.ws.store().run_state(&a.id)?.unwrap_or(RunState::Idle);
            let open: Vec<_> = tasks
                .iter()
                .filter(|t| t.assignee.as_deref() == Some(&a.id) && t.status != TaskStatus::Done)
                .map(|t| format!("{} {:?}", t.id, t.status))
                .collect();
            out.push(format!(
                "- {} (`{}`) role={}{} status={} tasks=[{}]",
                a.name,
                a.id,
                a.role_id,
                if a.is_lead { " lead" } else { "" },
                serde_json::to_value(&st).ok().and_then(|v| v["state"].as_str().map(str::to_string)).unwrap_or_default(),
                open.join(", ")
            ));
        }
        Ok(out.join("\n"))
    }
}

struct SpawnAgent;
#[async_trait]
impl Tool for SpawnAgent {
    fn name(&self) -> &'static str {
        "spawn_agent"
    }
    fn group(&self) -> &'static str {
        "team"
    }
    fn description(&self) -> String {
        "Create a new agent with a role (from this workspace or the library). It starts idle; give it work with assign_task.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["role"], "properties": {
            "role": {"type": "string"},
            "name": {"type": "string", "description": "display name (default: the role name)"}
        }})
    }
    async fn approval(&self, ctx: &ToolCtx, a: &Value) -> Option<String> {
        team_approval(ctx, format!("Spawn an agent with role `{}`", a.get("role").and_then(|v| v.as_str()).unwrap_or("?")))
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let role = s(&a, "role")?;
        let ag = ctx.ws.create_agent(role, opt_s(&a, "name"), Sender::agent(&ctx.agent.id), false)?;
        Ok(format!("Spawned {} (`{}`) with role {}.", ag.name, ag.id, ag.role_id))
    }
}

struct AssignTask;
#[async_trait]
impl Tool for AssignTask {
    fn name(&self) -> &'static str {
        "assign_task"
    }
    fn group(&self) -> &'static str {
        "team"
    }
    fn description(&self) -> String {
        "Put a task on the board and hand it to an agent. The agent starts working immediately; its final reply comes back to you as a report and wakes you up. Write the task so it can be done without asking you questions: context, exact deliverables, and testable acceptance criteria.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["agent", "title", "description", "acceptance_criteria"], "properties": {
            "agent": {"type": "string", "description": "agent id or name"},
            "title": {"type": "string"},
            "description": {"type": "string"},
            "acceptance_criteria": {"type": "string"},
            "task_id": {"type": "string", "description": "re-assign an existing task instead of creating one"}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let target = ctx.ws.find_agent(s(&a, "agent")?)?;
        if target.id == ctx.agent.id {
            bail!("you cannot assign a task to yourself; just do it");
        }
        let description = s(&a, "description")?;
        let acceptance = s(&a, "acceptance_criteria")?;
        let mut task = match opt_s(&a, "task_id") {
            Some(id) => {
                let mut t = ctx.ws.store().task(id)?.with_context(|| format!("no task {id}"))?;
                t.notes.push(TaskNote { by: Sender::agent(&ctx.agent.id), text: format!("Re-assigned to {}: {description}", target.name), at: now() });
                t.acceptance = acceptance.to_string();
                t
            }
            None => super::board::create_task(ctx, s(&a, "title")?, description, acceptance, Some(target.id.clone()))?,
        };
        task.assignee = Some(target.id.clone());
        task.status = TaskStatus::InProgress;
        task.updated_at = now();
        ctx.ws.emit(None, EventKind::TaskUpsert { task: task.clone() })?;
        let text = format!(
            "**{}**\n\n{description}\n\n**Acceptance criteria**\n{acceptance}\n\nUpdate the board with board_update(task_id=\"{}\") as you go, and end with a complete report.",
            task.title, task.id
        );
        ctx.ws.send_message(&target.id, Sender::agent(&ctx.agent.id), &text, MessageKind::TaskAssignment { task_id: task.id.clone() })?;
        Ok(format!(
            "Assigned {} to {} (`{}`). They are working on it now; their report will be delivered to you. If you have nothing else to do, end your turn with a short status update.",
            task.id, target.name, target.id
        ))
    }
}

struct MessageAgent;
#[async_trait]
impl Tool for MessageAgent {
    fn name(&self) -> &'static str {
        "message_agent"
    }
    fn group(&self) -> &'static str {
        "team"
    }
    fn description(&self) -> String {
        "Send a message to another agent (questions, feedback, extra context). It wakes the agent; its reply comes back to you as a report.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["agent", "text"], "properties": {
            "agent": {"type": "string"}, "text": {"type": "string"}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let target = ctx.ws.find_agent(s(&a, "agent")?)?;
        if target.id == ctx.agent.id {
            bail!("you cannot message yourself");
        }
        ctx.ws.send_message(&target.id, Sender::agent(&ctx.agent.id), s(&a, "text")?, MessageKind::Chat)?;
        Ok(format!("Message delivered to {}.", target.name))
    }
}

struct ReadTranscript;
#[async_trait]
impl Tool for ReadTranscript {
    fn name(&self) -> &'static str {
        "read_transcript"
    }
    fn group(&self) -> &'static str {
        "team"
    }
    fn description(&self) -> String {
        "Read the recent conversation and tool activity of another agent, to check how it reached its conclusions.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["agent"], "properties": {
            "agent": {"type": "string"},
            "last_events": {"type": "integer", "description": "default 40"}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let target = ctx.ws.find_agent(s(&a, "agent")?)?;
        let n = opt_u(&a, "last_events").unwrap_or(40).clamp(1, 400) as u32;
        let events = ctx.ws.store().agent_tail(&target.id, n)?;
        let agents = ctx.ws.agents()?;
        let text = crate::context::render_plain(&events, &agents, 1500);
        Ok(if text.trim().is_empty() { format!("{} has no activity yet.", target.name) } else { kk_llm::truncate(&text, 60_000) })
    }
}
