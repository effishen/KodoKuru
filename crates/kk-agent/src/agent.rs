//! The agent loop.
//!
//! One run = repeatedly: rebuild history from the event log → call the
//! engine (streaming) → execute tool calls → repeat, until the agent gives a
//! final reply that survives KodoKuru's checks:
//!
//! - **checklist nudge**: open checklist items must be finished or removed;
//! - **completion self-check**: the model judges its reply against the
//!   request (COMPLETE / CONTINUE / NEEDS_INPUT), bounded per run;
//! - **new input**: messages that arrived during the run are answered first.
//!
//! The final reply is delivered as a report to agents that sent work. Runs
//! survive engine outages (wait and retry), restarts (resume from the log)
//! and context limits (compaction).

use anyhow::{anyhow, Context, Result};
use kk_core::*;
use kk_llm::{ChatMessage, ChatRequest, ChatResponse, LlmError, StreamEvent};
use std::sync::Arc;
use std::time::Duration;

use crate::context;
use crate::prompt;
use crate::tools::{self, Tool, ToolCtx};
use crate::workspace::{RunCtl, StopReason, Workspace};
use crate::Live;

const MAX_TOOL_OUTPUT: usize = 60_000;
const MAX_CHECKLIST_NUDGES: u32 = 2;
const MAX_SELF_CHECKS: u32 = 2;

enum Exit {
    Cancelled,
    Paused(String),
    Locked,
}

#[derive(Debug)]
enum Abort {
    Exit(ExitKind),
    Fail(anyhow::Error),
}

#[derive(Debug)]
enum ExitKind {
    Cancelled,
    Locked,
    Paused(String),
}

impl From<anyhow::Error> for Abort {
    fn from(e: anyhow::Error) -> Self {
        Abort::Fail(e)
    }
}

pub async fn run(ws: Arc<Workspace>, agent_id: String, ctl: RunCtl) {
    let result = run_inner(&ws, &agent_id, &ctl).await;
    match result {
        Ok(()) => {}
        Err(Abort::Exit(ExitKind::Cancelled)) => match ctl.stop_reason() {
            StopReason::User => {
                ws.set_run_state(&agent_id, RunState::Stopped);
                ws.notice(Some(&agent_id), "info", "Stopped by the user.");
            }
            StopReason::Lock => ws.set_run_state(&agent_id, RunState::Locked),
            // Leave the state as it was so the run resumes next start.
            StopReason::Shutdown | StopReason::None => {}
        },
        Err(Abort::Exit(ExitKind::Locked)) => ws.set_run_state(&agent_id, RunState::Locked),
        Err(Abort::Exit(ExitKind::Paused(reason))) => {
            ws.notice(Some(&agent_id), "warn", format!("Paused: {reason}. Send a message to continue."));
            ws.set_run_state(&agent_id, RunState::Paused { reason });
        }
        Err(Abort::Fail(e)) => {
            tracing::error!("agent {agent_id} failed: {e:#}");
            ws.notice(Some(&agent_id), "error", format!("{e:#}"));
            ws.set_run_state(&agent_id, RunState::Error { message: format!("{e:#}") });
        }
    }
    ws.end_run(&agent_id, &ctl);
}

fn exit<T>(e: Exit) -> Result<T, Abort> {
    Err(Abort::Exit(match e {
        Exit::Cancelled => ExitKind::Cancelled,
        Exit::Paused(r) => ExitKind::Paused(r),
        Exit::Locked => ExitKind::Locked,
    }))
}

/// Seq of the agent's last final reply (assistant turn without tool calls).
fn last_final_seq(ws: &Workspace, agent_id: &str) -> Result<i64> {
    let events = ws.store().agent_events(agent_id, 0)?;
    Ok(events
        .iter()
        .rev()
        .find(|e| matches!(&e.kind, EventKind::Assistant { tool_calls, .. } if tool_calls.is_empty()))
        .map(|e| e.seq)
        .unwrap_or(0))
}

async fn run_inner(ws: &Arc<Workspace>, agent_id: &str, ctl: &RunCtl) -> Result<(), Abort> {
    let store = ws.store();
    let agent = store.agent(agent_id)?.with_context(|| format!("no agent {agent_id}"))?;
    let mut first = true;
    loop {
        if !first && !context::has_unprocessed_input(store, agent_id)? {
            // Spurious wake: nothing new to answer.
            if ws.finish_or_continue(agent_id, ctl) {
                continue;
            }
            return Ok(());
        }
        first = false;
        if ws.rt.is_locked().await {
            return exit(Exit::Locked);
        }
        ws.set_run_state(agent_id, RunState::Running);
        let inputs_after = last_final_seq(ws, agent_id)?;
        let final_text = run_turns(ws, &agent, ctl).await?;
        deliver_reports(ws, &agent, inputs_after, &final_text)?;
        ws.set_run_state(agent_id, RunState::Idle);
        if !ws.finish_or_continue(agent_id, ctl) {
            return Ok(());
        }
    }
}

/// Run model/tool turns until a final reply is accepted. Returns its text.
async fn run_turns(ws: &Arc<Workspace>, agent: &AgentInfo, ctl: &RunCtl) -> Result<String, Abort> {
    let store = ws.store();
    let rt = &ws.rt;
    let role = ws.ensure_role(&agent.role_id)?;
    let settings = ws.settings();
    let engine = rt.chat_engine_for(&role, &settings).await?;
    let key = rt.resolve_key(&engine.api_key).await?;
    let tctx = ToolCtx { ws: ws.clone(), agent: agent.clone(), role: role.clone(), cancel: ctl.cancel.clone() };
    let tools = tools::for_role(&tctx).await;
    let text_mode = engine.tool_call_mode == ToolCallMode::Text;
    let envs = ws.env_recipes().unwrap_or_default();
    let team = ws.agents()?;
    let manifest = ws.manifest();
    let default_env = role.environment.clone().or(settings.default_environment.clone());
    let system = prompt::system_prompt(&prompt::PromptInput {
        manifest: &manifest,
        agent,
        role: &role,
        envs: &envs,
        default_env: default_env.as_deref(),
        team: &team,
        tool_text: text_mode.then(|| tools::text_instructions(&tools)),
    });
    let specs = if text_mode { vec![] } else { tools::specs(&tools) };
    let tools_chars = serde_json::to_string(&specs).map(|s| s.len()).unwrap_or(0);
    let ctx_window = engine.context_window.max(2048) as usize;
    let max_out = (engine.max_output_tokens as usize).min(ctx_window / 2).max(256);
    let budget = (ctx_window - max_out) * 85 / 100;

    let mut steps = 0u32;
    let mut nudges = 0u32;
    let mut checks = 0u32;
    let mut empty_replies = 0u32;
    let mut did_work = false;
    let mut compacted_for_overflow = false;

    loop {
        if ctl.cancel.is_cancelled() {
            return exit(Exit::Cancelled);
        }
        let agents = ws.agents()?;
        let hist = context::load(store, &agent.id)?;
        let msgs = context::to_messages(&hist, &agents, text_mode);
        if context::approx_tokens(&msgs, &system, tools_chars) > budget {
            compact(ws, agent, &engine, key.as_deref(), ctl).await?;
            continue;
        }
        let mut messages = Vec::with_capacity(msgs.len() + 1);
        messages.push(ChatMessage::system(system.clone()));
        messages.extend(msgs);
        let req = ChatRequest {
            messages,
            tools: specs.clone(),
            max_tokens: Some(max_out as u32),
            temperature: role.temperature,
            reasoning_effort: role.reasoning_effort.clone(),
            json_mode: false,
        };
        let resp = match call_llm(ws, &agent.id, &engine, key.as_deref(), &req, ctl, true).await {
            Ok(r) => r,
            Err(Abort::Fail(e)) if e.downcast_ref::<Overflow>().is_some() && !compacted_for_overflow => {
                compacted_for_overflow = true;
                ws.notice(Some(&agent.id), "info", "The engine reported the context is too long; compacting.");
                compact(ws, agent, &engine, key.as_deref(), ctl).await?;
                continue;
            }
            Err(e) => return Err(e),
        };
        compacted_for_overflow = false;
        ws.emit(
            Some(&agent.id),
            EventKind::Assistant {
                content: resp.content.clone(),
                reasoning: resp.reasoning.clone(),
                tool_calls: resp.tool_calls.clone(),
                usage: resp.usage.clone(),
                engine: engine.id.clone(),
            },
        )?;

        if !resp.tool_calls.is_empty() {
            did_work = true;
            for call in &resp.tool_calls {
                if ctl.cancel.is_cancelled() {
                    return exit(Exit::Cancelled);
                }
                let (output, is_error) = execute_tool(ws, &tctx, &tools, call).await?;
                ws.emit(
                    Some(&agent.id),
                    EventKind::ToolResult { call_id: call.id.clone(), name: call.name.clone(), output, is_error },
                )?;
            }
            steps += 1;
            if steps >= settings.max_steps_per_run {
                return exit(Exit::Paused(format!("reached the limit of {} tool steps in one run", settings.max_steps_per_run)));
            }
            continue;
        }

        // ---- candidate final reply
        if resp.content.trim().is_empty() {
            empty_replies += 1;
            if empty_replies <= 2 {
                harness(ws, &agent.id, "Your last reply was empty. Continue the task, or give your final report.")?;
                continue;
            }
        }
        if context::has_unprocessed_input(store, &agent.id)? {
            // Messages arrived while we were working: answer them too.
            ctl.again.store(false, std::sync::atomic::Ordering::SeqCst);
            continue;
        }
        let open: Vec<ChecklistItem> =
            open_items(&store.checklist(&agent.id)?).into_iter().cloned().collect();
        if !open.is_empty() && nudges < MAX_CHECKLIST_NUDGES {
            nudges += 1;
            let list = open.iter().map(|i| format!("- {}", i.content)).collect::<Vec<_>>().join("\n");
            harness(
                ws,
                &agent.id,
                &format!(
                    "Your checklist still has {} open item(s):\n{list}\nContinue with the next one. If an item is no longer needed, update the checklist (mark items completed only when verified). Then give your final report.",
                    open.len()
                ),
            )?;
            continue;
        }
        if role.self_check && did_work && checks < MAX_SELF_CHECKS {
            checks += 1;
            match self_check(ws, agent, &engine, key.as_deref(), &system, &specs, text_mode, ctl).await? {
                Verdict::Continue { unmet, next } => {
                    harness(
                        ws,
                        &agent.id,
                        &format!("Completion check: not finished yet.\nUnmet: {unmet}\nNext: {next}\nContinue working, then give your final report."),
                    )?;
                    continue;
                }
                Verdict::Complete | Verdict::NeedsInput | Verdict::Unknown => {}
            }
        }
        return Ok(resp.content);
    }
}

fn harness(ws: &Workspace, agent_id: &str, text: &str) -> Result<()> {
    // Emitted directly (not send_message) so it doesn't re-wake this run.
    ws.emit(
        Some(agent_id),
        EventKind::Message { to: agent_id.into(), from: Sender::Harness, text: text.into(), kind: MessageKind::Harness },
    )?;
    Ok(())
}

#[derive(Debug)]
struct Overflow;
impl std::fmt::Display for Overflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "context too long")
    }
}
impl std::error::Error for Overflow {}

/// Call the engine, waiting for a free slot and retrying while it is
/// offline. Streams deltas to the UI when `stream_ui` is set.
async fn call_llm(
    ws: &Arc<Workspace>,
    agent_id: &str,
    engine: &EngineConfig,
    key: Option<&str>,
    req: &ChatRequest,
    ctl: &RunCtl,
    stream_ui: bool,
) -> Result<ChatResponse, Abort> {
    let rt = ws.rt.clone();
    let mut backoff = Duration::from_secs(3);
    loop {
        let sem = rt.slot(engine);
        let permit = match sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                ws.set_run_state(agent_id, RunState::WaitingEngine { engine: engine.id.clone(), reason: "busy with another agent".into() });
                tokio::select! {
                    p = sem.acquire_owned() => p.map_err(|e| anyhow!("{e}"))?,
                    _ = ctl.cancel.cancelled() => return exit(Exit::Cancelled),
                }
            }
        };
        ws.set_run_state(agent_id, RunState::Running);
        let wid = ws.id().to_string();
        let aid = agent_id.to_string();
        let rt2 = rt.clone();
        let mut on_event = move |ev: StreamEvent| {
            if !stream_ui {
                return;
            }
            let (text, reasoning) = match ev {
                StreamEvent::Text(t) => (Some(t), None),
                StreamEvent::Reasoning(r) => (None, Some(r)),
            };
            rt2.publish(Some(&wid), Live::Delta { agent_id: aid.clone(), text, reasoning });
        };
        let r = rt.client.chat(engine, key, req, &mut on_event, &ctl.cancel).await;
        drop(permit);
        match r {
            Ok(resp) => return Ok(resp),
            Err(LlmError::Cancelled) => return exit(Exit::Cancelled),
            Err(LlmError::Unreachable(msg)) => {
                tracing::warn!("engine {} unreachable: {msg}", engine.id);
                ws.set_run_state(agent_id, RunState::WaitingEngine { engine: engine.id.clone(), reason: kk_llm::truncate(&msg, 200) });
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = ctl.cancel.cancelled() => return exit(Exit::Cancelled),
                }
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
            Err(LlmError::ContextTooLong(_)) => return Err(Abort::Fail(anyhow::Error::new(Overflow))),
            Err(LlmError::Rejected(code, body)) => {
                return Err(Abort::Fail(anyhow!("engine {} rejected the request (HTTP {code}): {body}", engine.id)))
            }
        }
    }
}

async fn execute_tool(ws: &Arc<Workspace>, ctx: &ToolCtx, tools: &[Arc<dyn Tool>], call: &ToolCall) -> Result<(String, bool), Abort> {
    let Some(tool) = tools.iter().find(|t| t.name() == call.name) else {
        let names = tools.iter().map(|t| t.name()).collect::<Vec<_>>().join(", ");
        return Ok((format!("Unknown tool `{}`. Available tools: {names}", call.name), true));
    };
    let args = match tools::parse_args(call) {
        Ok(a) => a,
        Err(e) => return Ok((e, true)),
    };
    if let Some(summary) = tool.approval(ctx, &args).await {
        match ws.request_approval(&ctx.agent.id, call, summary, &ctx.cancel).await {
            Ok((true, _)) => {}
            Ok((false, note)) => {
                let note = if note.is_empty() { String::new() } else { format!(" Note from the user: {note}") };
                return Ok((
                    format!("The user denied this action.{note} Do not retry it unchanged; adjust your plan or ask the user."),
                    true,
                ));
            }
            Err(_) => return exit(Exit::Cancelled),
        }
    }
    let fut = tool.run(ctx, args);
    let r = tokio::select! {
        r = fut => r,
        _ = ctx.cancel.cancelled() => return exit(Exit::Cancelled),
    };
    Ok(match r {
        Ok(out) => (cap(&out), false),
        Err(e) => (cap(&format!("Error: {e:#}")), true),
    })
}

fn cap(s: &str) -> String {
    if s.len() <= MAX_TOOL_OUTPUT {
        return s.to_string();
    }
    kk_env::cap_output(s.as_bytes(), MAX_TOOL_OUTPUT)
}

enum Verdict {
    Complete,
    Continue { unmet: String, next: String },
    NeedsInput,
    Unknown,
}

#[allow(clippy::too_many_arguments)]
async fn self_check(
    ws: &Arc<Workspace>,
    agent: &AgentInfo,
    engine: &EngineConfig,
    key: Option<&str>,
    system: &str,
    specs: &[kk_llm::ToolSpec],
    text_mode: bool,
    ctl: &RunCtl,
) -> Result<Verdict, Abort> {
    let agents = ws.agents()?;
    let hist = context::load(ws.store(), &agent.id)?;
    let mut messages = vec![ChatMessage::system(system.to_string())];
    messages.extend(context::to_messages(&hist, &agents, text_mode));
    messages.push(ChatMessage::user(prompt::COMPLETION_CHECK));
    let req = ChatRequest {
        messages,
        tools: specs.to_vec(),
        max_tokens: Some(1024),
        temperature: Some(0.0),
        reasoning_effort: None,
        json_mode: false,
    };
    let resp = match call_llm(ws, &agent.id, engine, key, &req, ctl, false).await {
        Ok(r) => r,
        Err(Abort::Fail(e)) if e.downcast_ref::<Overflow>().is_some() => return Ok(Verdict::Unknown),
        Err(e) => return Err(e),
    };
    let v = parse_verdict(&resp);
    let label = match &v {
        Verdict::Complete => "COMPLETE".to_string(),
        Verdict::NeedsInput => "NEEDS_INPUT".to_string(),
        Verdict::Unknown => "unclear (accepted)".to_string(),
        Verdict::Continue { unmet, .. } => format!("CONTINUE — {unmet}"),
    };
    let _ = ws.emit(Some(&agent.id), EventKind::Notice { level: "check".into(), text: format!("Completion check: {label}") });
    Ok(v)
}

fn parse_verdict(resp: &ChatResponse) -> Verdict {
    if !resp.tool_calls.is_empty() {
        return Verdict::Continue { unmet: "the agent wants to keep working".into(), next: "continue with the planned tool calls".into() };
    }
    let text = resp.content.trim();
    let json = text.find('{').and_then(|a| text.rfind('}').map(|b| &text[a..=b]));
    if let Some(v) = json.and_then(kk_llm::repair_json).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) {
        let status = v["status"].as_str().unwrap_or("").to_ascii_uppercase();
        let field = |k: &str| v[k].as_str().unwrap_or("").trim().to_string();
        return match status.as_str() {
            "COMPLETE" => Verdict::Complete,
            "NEEDS_INPUT" => Verdict::NeedsInput,
            "CONTINUE" => {
                let unmet = field("unmet_requirement");
                let next = field("next_action");
                if unmet.is_empty() && next.is_empty() {
                    Verdict::Complete
                } else {
                    Verdict::Continue { unmet, next }
                }
            }
            _ => Verdict::Unknown,
        };
    }
    let up = text.to_ascii_uppercase();
    if up.contains("NEEDS_INPUT") {
        Verdict::NeedsInput
    } else if up.contains("CONTINUE") && !up.contains("COMPLETE") {
        Verdict::Continue { unmet: kk_llm::truncate(text, 300), next: "address the unmet requirement".into() }
    } else {
        Verdict::Unknown
    }
}

/// Summarise older history so the conversation fits the context window.
async fn compact(ws: &Arc<Workspace>, agent: &AgentInfo, engine: &EngineConfig, key: Option<&str>, ctl: &RunCtl) -> Result<(), Abort> {
    let hist = context::load(ws.store(), &agent.id)?;
    let ctx_chars = engine.context_window as usize * 7 / 2;
    let cut = context::choose_cut(&hist.events, ctx_chars / 4).filter(|c| *c > 0);
    let Some(cut) = cut else {
        return Err(Abort::Fail(anyhow!("the conversation cannot be compacted further (a single message is larger than the context window)")));
    };
    ws.notice(Some(&agent.id), "info", "Compacting older context into a summary…");
    let agents = ws.agents()?;
    let mut transcript = context::render_plain(&hist.events[..cut], &agents, 2000);
    let room = ctx_chars * 55 / 100;
    if transcript.len() > room {
        let mut start = transcript.len() - room;
        while !transcript.is_char_boundary(start) {
            start += 1;
        }
        transcript = format!("[… earliest part omitted …]\n{}", &transcript[start..]);
    }
    let mut user = String::new();
    if let Some(s) = &hist.summary {
        user.push_str(&format!("## Earlier summary\n{s}\n\n"));
    }
    user.push_str(&format!("## Conversation to summarise\n{transcript}\n\n{}", prompt::COMPACTION));
    let req = ChatRequest {
        messages: vec![
            ChatMessage::system(format!(
                "You maintain the working memory of {} ({}), an agent in KodoKuru. You write precise, complete summaries.",
                agent.name, agent.role_id
            )),
            ChatMessage::user(user),
        ],
        tools: vec![],
        max_tokens: Some(engine.max_output_tokens.min(4096)),
        temperature: Some(0.2),
        reasoning_effort: None,
        json_mode: false,
    };
    let resp = call_llm(ws, &agent.id, engine, key, &req, ctl, false).await?;
    let summary = resp.content.trim();
    if summary.is_empty() {
        return Err(Abort::Fail(anyhow!("compaction produced an empty summary")));
    }
    ws.emit(Some(&agent.id), EventKind::Compaction { summary: summary.to_string(), upto_seq: hist.events[cut - 1].seq })?;
    Ok(())
}

/// Deliver the final reply to agents that sent this agent work or questions
/// since its previous final reply, and move assigned tasks to review.
fn deliver_reports(ws: &Arc<Workspace>, agent: &AgentInfo, after_seq: i64, text: &str) -> Result<()> {
    let events = ws.store().agent_events(&agent.id, after_seq)?;
    let mut senders: Vec<String> = Vec::new();
    let mut tasks: Vec<String> = Vec::new();
    for e in &events {
        if let EventKind::Message { from: Sender::Agent { id }, kind, .. } = &e.kind {
            match kind {
                MessageKind::Chat => {}
                MessageKind::TaskAssignment { task_id } => tasks.push(task_id.clone()),
                // Replies to reports don't bounce back (no ping-pong).
                MessageKind::Report | MessageKind::Harness => continue,
            }
            if id != &agent.id && !senders.contains(id) {
                senders.push(id.clone());
            }
        }
    }
    for tid in &tasks {
        if let Some(mut t) = ws.store().task(tid)? {
            if t.status == TaskStatus::InProgress || t.status == TaskStatus::Todo {
                t.status = TaskStatus::Review;
                t.notes.push(TaskNote { by: Sender::Harness, text: format!("{} reported back.", agent.name), at: now() });
                t.updated_at = now();
                ws.emit(None, EventKind::TaskUpsert { task: t })?;
            }
        }
    }
    let body = if text.trim().is_empty() { "(no report text)".to_string() } else { text.to_string() };
    let body = if tasks.is_empty() { body } else { format!("Tasks: {}\n\n{body}", tasks.join(", ")) };
    for s in senders {
        if ws.store().agent(&s)?.is_some() {
            ws.send_message(&s, Sender::agent(&agent.id), &body, MessageKind::Report)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(c: &str) -> ChatResponse {
        ChatResponse { content: c.into(), ..Default::default() }
    }

    #[test]
    fn verdicts() {
        assert!(matches!(parse_verdict(&resp(r#"{"status":"COMPLETE"}"#)), Verdict::Complete));
        assert!(matches!(
            parse_verdict(&resp("Sure:\n```json\n{\"status\": \"CONTINUE\", \"unmet_requirement\": \"tests\", \"next_action\": \"run them\"}\n```")),
            Verdict::Continue { .. }
        ));
        assert!(matches!(parse_verdict(&resp(r#"{"status":"CONTINUE"}"#)), Verdict::Complete));
        assert!(matches!(parse_verdict(&resp("NEEDS_INPUT")), Verdict::NeedsInput));
        assert!(matches!(parse_verdict(&resp("whatever")), Verdict::Unknown));
    }
}
