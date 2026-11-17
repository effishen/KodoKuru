//! The personal checklist and the shared task board.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use kk_core::*;
use serde_json::{json, Value};
use std::sync::Arc;

use super::{opt_s, s, Tool, ToolCtx};

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(UpdateChecklist), Arc::new(BoardList), Arc::new(BoardCreate), Arc::new(BoardUpdate)]
}

pub fn render_checklist(items: &[ChecklistItem]) -> String {
    if items.is_empty() {
        return "(checklist is empty)".into();
    }
    items
        .iter()
        .map(|i| {
            let mark = match i.status {
                ChecklistStatus::Completed => "[x]",
                ChecklistStatus::InProgress => "[>]",
                ChecklistStatus::Pending => "[ ]",
            };
            format!("{mark} {}", i.content)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_task(t: &Task, agents: &[AgentInfo]) -> String {
    let who = t
        .assignee
        .as_ref()
        .map(|a| agents.iter().find(|x| &x.id == a).map(|x| x.name.clone()).unwrap_or(a.clone()))
        .unwrap_or_else(|| "unassigned".into());
    let mut s = format!("{} [{:?}] {} — {who}", t.id, t.status, t.title);
    if !t.acceptance.is_empty() {
        s.push_str(&format!("\n  acceptance: {}", t.acceptance.replace('\n', "\n    ")));
    }
    if let Some(n) = t.notes.last() {
        s.push_str(&format!("\n  latest note: {}", kk_llm::truncate(&n.text, 300)));
    }
    s
}

struct UpdateChecklist;
#[async_trait]
impl Tool for UpdateChecklist {
    fn name(&self) -> &'static str {
        "update_checklist"
    }
    fn group(&self) -> &'static str {
        "checklist"
    }
    fn description(&self) -> String {
        "Replace your checklist with the full, current list of steps. Use it to plan before starting and update it as you go: exactly one item in_progress at a time, mark items completed only after verifying them. The user sees this checklist live.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["items"], "properties": {
            "items": {"type": "array", "items": {"type": "object", "required": ["content", "status"], "properties": {
                "content": {"type": "string"},
                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
            }}}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let items: Vec<ChecklistItem> = serde_json::from_value(a.get("items").cloned().context("missing `items`")?)
            .context("items must be [{content, status: pending|in_progress|completed}]")?;
        validate_checklist(&items).map_err(|e| anyhow::anyhow!(e))?;
        ctx.ws.emit(Some(&ctx.agent.id), EventKind::Checklist { items: items.clone() })?;
        let open = open_items(&items).len();
        Ok(format!("Checklist updated ({open} open):\n{}", render_checklist(&items)))
    }
}

struct BoardList;
#[async_trait]
impl Tool for BoardList {
    fn name(&self) -> &'static str {
        "board_list"
    }
    fn group(&self) -> &'static str {
        "board"
    }
    fn description(&self) -> String {
        "List tasks on the shared board, optionally filtered by status or assignee. Pass task_id for full details and history.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "status": {"type": "string", "enum": ["todo", "in_progress", "review", "done", "blocked"]},
            "assignee": {"type": "string", "description": "agent id or name; 'me' for yourself"},
            "task_id": {"type": "string"}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let agents = ctx.ws.agents()?;
        if let Some(id) = opt_s(&a, "task_id") {
            let t = ctx.ws.store().task(id)?.with_context(|| format!("no task {id}"))?;
            let mut s = render_task(&t, &agents);
            if !t.description.is_empty() {
                s.push_str(&format!("\n  description: {}", t.description));
            }
            for n in &t.notes {
                let by = match &n.by {
                    Sender::Agent { id } => agents.iter().find(|x| &x.id == id).map(|x| x.name.clone()).unwrap_or(id.clone()),
                    Sender::User => "user".into(),
                    Sender::Harness => "KodoKuru".into(),
                };
                s.push_str(&format!("\n  - {by}: {}", n.text));
            }
            return Ok(s);
        }
        let status = opt_s(&a, "status").and_then(TaskStatus::parse);
        let assignee = match opt_s(&a, "assignee") {
            Some("me") => Some(ctx.agent.id.clone()),
            Some(x) => Some(ctx.ws.find_agent(x)?.id),
            None => None,
        };
        let tasks: Vec<_> = ctx
            .ws
            .store()
            .tasks()?
            .into_iter()
            .filter(|t| status.is_none_or(|s| t.status == s))
            .filter(|t| assignee.as_ref().is_none_or(|a| t.assignee.as_ref() == Some(a)))
            .collect();
        if tasks.is_empty() {
            return Ok("No matching tasks.".into());
        }
        Ok(tasks.iter().map(|t| render_task(t, &agents)).collect::<Vec<_>>().join("\n"))
    }
}

pub(crate) fn create_task(ctx: &ToolCtx, title: &str, description: &str, acceptance: &str, assignee: Option<String>) -> Result<Task> {
    let t = Task {
        id: ctx.ws.store().next_task_id()?,
        title: title.trim().to_string(),
        description: description.to_string(),
        acceptance: acceptance.to_string(),
        status: TaskStatus::Todo,
        assignee,
        created_by: Sender::agent(&ctx.agent.id),
        notes: vec![],
        created_at: now(),
        updated_at: now(),
    };
    if t.title.is_empty() {
        bail!("task needs a title");
    }
    ctx.ws.emit(None, EventKind::TaskUpsert { task: t.clone() })?;
    Ok(t)
}

struct BoardCreate;
#[async_trait]
impl Tool for BoardCreate {
    fn name(&self) -> &'static str {
        "board_create"
    }
    fn group(&self) -> &'static str {
        "board"
    }
    fn description(&self) -> String {
        "Add a task to the board without assigning it (use assign_task to hand work to an agent).".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["title"], "properties": {
            "title": {"type": "string"},
            "description": {"type": "string"},
            "acceptance_criteria": {"type": "string"}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let t = create_task(
            ctx,
            s(&a, "title")?,
            opt_s(&a, "description").unwrap_or(""),
            opt_s(&a, "acceptance_criteria").unwrap_or(""),
            None,
        )?;
        Ok(format!("Created {}: {}", t.id, t.title))
    }
}

struct BoardUpdate;
#[async_trait]
impl Tool for BoardUpdate {
    fn name(&self) -> &'static str {
        "board_update"
    }
    fn group(&self) -> &'static str {
        "board"
    }
    fn description(&self) -> String {
        "Change a task's status and/or add a progress note. Statuses: todo, in_progress, review (ready to be checked), done (verified), blocked.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["task_id"], "properties": {
            "task_id": {"type": "string"},
            "status": {"type": "string", "enum": ["todo", "in_progress", "review", "done", "blocked"]},
            "note": {"type": "string"}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let id = s(&a, "task_id")?;
        let mut t = ctx.ws.store().task(id)?.with_context(|| format!("no task {id}"))?;
        if let Some(st) = opt_s(&a, "status") {
            t.status = TaskStatus::parse(st).with_context(|| format!("unknown status {st:?}"))?;
        }
        if let Some(n) = opt_s(&a, "note") {
            t.notes.push(TaskNote { by: Sender::agent(&ctx.agent.id), text: n.to_string(), at: now() });
        }
        t.updated_at = now();
        ctx.ws.emit(None, EventKind::TaskUpsert { task: t.clone() })?;
        Ok(format!("{} is now {:?}", t.id, t.status))
    }
}
