//! Rebuilds an agent's chat history from the event log, and decides where to
//! cut when the history is compacted.

use anyhow::Result;
use kk_core::*;
use kk_llm::ChatMessage;
use kk_store::Store;
use std::collections::HashSet;

pub const INTERRUPTED: &str = "[KodoKuru] This tool call was interrupted (KodoKuru restarted or the run was stopped) before it finished. Its result is unknown: check the current state before repeating it.";

/// The agent's timeline after its latest compaction, plus that summary.
pub struct History {
    pub summary: Option<String>,
    pub events: Vec<Event>,
}

pub fn load(store: &Store, agent_id: &str) -> Result<History> {
    let all = store.agent_events(agent_id, 0)?;
    let mut summary = None;
    let mut cut = 0i64;
    for e in &all {
        if let EventKind::Compaction { summary: s, upto_seq } = &e.kind {
            summary = Some(s.clone());
            cut = *upto_seq;
        }
    }
    let events = all
        .into_iter()
        .filter(|e| e.seq > cut)
        .filter(|e| matches!(e.kind, EventKind::Message { .. } | EventKind::Assistant { .. } | EventKind::ToolResult { .. }))
        .collect();
    Ok(History { summary, events })
}

fn agent_label(agents: &[AgentInfo], id: &str) -> String {
    agents
        .iter()
        .find(|a| a.id == id)
        .map(|a| format!("{} ({})", a.name, a.role_id))
        .unwrap_or_else(|| id.to_string())
}

/// Text of an incoming message as the model sees it.
pub fn render_message(agents: &[AgentInfo], from: &Sender, text: &str, kind: &MessageKind) -> String {
    match (from, kind) {
        (_, MessageKind::Harness) | (Sender::Harness, _) => format!("[KodoKuru] {text}"),
        (Sender::User, _) => text.to_string(),
        (Sender::Agent { id }, MessageKind::TaskAssignment { task_id }) => {
            format!("[Task {task_id} assigned to you by {}]\n{text}", agent_label(agents, id))
        }
        (Sender::Agent { id }, MessageKind::Report) => format!("[Report from {}]\n{text}", agent_label(agents, id)),
        (Sender::Agent { id }, MessageKind::Chat) => format!("[Message from {}]\n{text}", agent_label(agents, id)),
    }
}

fn text_tool_call(c: &ToolCall) -> String {
    let args: serde_json::Value = serde_json::from_str(&c.arguments).unwrap_or(serde_json::Value::String(c.arguments.clone()));
    format!("<tool_call>\n{}\n</tool_call>", serde_json::json!({"name": c.name, "arguments": args}))
}

/// Convert events into chat messages (system prompt not included).
///
/// Guarantees backends' expectations: every assistant tool call gets a
/// result (synthesised as "interrupted" if missing), orphan results are
/// dropped, consecutive user messages are merged, and the first message is
/// from the user.
pub fn to_messages(h: &History, agents: &[AgentInfo], text_mode: bool) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::new();
    if let Some(s) = &h.summary {
        out.push(ChatMessage::user(format!(
            "[KodoKuru] Summary of the earlier part of this conversation (older messages were compacted):\n\n{s}"
        )));
    }
    let mut open_calls: Vec<ToolCall> = Vec::new();
    let mut answered: HashSet<String> = HashSet::new();

    fn close_calls(out: &mut Vec<ChatMessage>, open: &mut Vec<ToolCall>, answered: &mut HashSet<String>, text_mode: bool) {
        for c in open.drain(..) {
            if !answered.contains(&c.id) {
                push_tool_result(out, &c.id, &c.name, INTERRUPTED, text_mode);
            }
        }
        answered.clear();
    }

    for e in &h.events {
        match &e.kind {
            EventKind::Message { from, text, kind, .. } => {
                close_calls(&mut out, &mut open_calls, &mut answered, text_mode);
                push_user(&mut out, render_message(agents, from, text, kind));
            }
            EventKind::Assistant { content, tool_calls, .. } => {
                close_calls(&mut out, &mut open_calls, &mut answered, text_mode);
                if text_mode {
                    let mut c = content.clone();
                    for tc in tool_calls {
                        if !c.is_empty() {
                            c.push('\n');
                        }
                        c.push_str(&text_tool_call(tc));
                    }
                    out.push(ChatMessage::assistant(c, &[]));
                } else {
                    out.push(ChatMessage::assistant(content.clone(), tool_calls));
                }
                open_calls = tool_calls.clone();
            }
            EventKind::ToolResult { call_id, name, output, .. } => {
                if open_calls.iter().any(|c| &c.id == call_id) && answered.insert(call_id.clone()) {
                    push_tool_result(&mut out, call_id, name, output, text_mode);
                }
            }
            _ => {}
        }
    }
    close_calls(&mut out, &mut open_calls, &mut answered, text_mode);
    if out.first().map(|m| m.role != "user").unwrap_or(false) {
        out.insert(0, ChatMessage::user("[KodoKuru] (continuing earlier work)"));
    }
    out
}

fn push_user(out: &mut Vec<ChatMessage>, text: String) {
    if let Some(last) = out.last_mut() {
        if last.role == "user" {
            let c = last.content.get_or_insert_with(String::new);
            c.push_str("\n\n");
            c.push_str(&text);
            return;
        }
    }
    out.push(ChatMessage::user(text));
}

fn push_tool_result(out: &mut Vec<ChatMessage>, call_id: &str, name: &str, output: &str, text_mode: bool) {
    if text_mode {
        push_user(out, format!("[Tool result: {name}]\n{output}"));
    } else {
        out.push(ChatMessage::tool(call_id, output));
    }
}

/// True when the agent has input it has not answered yet, or tool calls
/// whose results never arrived.
pub fn has_unprocessed_input(store: &Store, agent_id: &str) -> Result<bool> {
    let h = load(store, agent_id)?;
    Ok(match h.events.last().map(|e| &e.kind) {
        Some(EventKind::Message { .. }) => true,
        Some(EventKind::Assistant { tool_calls, .. }) => !tool_calls.is_empty(),
        Some(EventKind::ToolResult { .. }) => true,
        _ => false,
    })
}

pub fn approx_tokens(messages: &[ChatMessage], system: &str, tools_chars: usize) -> usize {
    let chars: usize = messages.iter().map(|m| m.approx_chars()).sum::<usize>() + system.len() + tools_chars;
    // ~3.5 characters per token is a conservative average for mixed text/code.
    chars * 2 / 7 + 16
}

/// Choose where to compact: returns the index of the first event to keep.
/// The cut is placed at an incoming message so tool-call pairs stay intact,
/// keeping roughly `keep_chars` of the most recent history.
pub fn choose_cut(events: &[Event], keep_chars: usize) -> Option<usize> {
    let size = |e: &Event| -> usize {
        match &e.kind {
            EventKind::Message { text, .. } => text.len(),
            EventKind::Assistant { content, tool_calls, .. } => {
                content.len() + tool_calls.iter().map(|c| c.arguments.len() + 32).sum::<usize>()
            }
            EventKind::ToolResult { output, .. } => output.len(),
            _ => 0,
        }
    };
    let mut tail = 0usize;
    let mut best = None;
    for i in (1..events.len()).rev() {
        tail += size(&events[i]);
        if matches!(events[i].kind, EventKind::Message { .. }) {
            if tail <= keep_chars || best.is_none() {
                best = Some(i);
            }
            if tail > keep_chars {
                break;
            }
        }
    }
    best
}

/// Render events as plain text (for summaries and `read_transcript`).
pub fn render_plain(events: &[Event], agents: &[AgentInfo], max_tool_chars: usize) -> String {
    let mut s = String::new();
    for e in events {
        match &e.kind {
            EventKind::Message { from, text, kind, .. } => {
                let who = match from {
                    Sender::User => "User".to_string(),
                    Sender::Harness => "KodoKuru".to_string(),
                    Sender::Agent { id } => agent_label(agents, id),
                };
                let _ = kind;
                s.push_str(&format!("### {who}\n{text}\n\n"));
            }
            EventKind::Assistant { content, tool_calls, .. } => {
                if !content.trim().is_empty() {
                    s.push_str(&format!("### Assistant\n{}\n\n", content.trim()));
                }
                for c in tool_calls {
                    s.push_str(&format!("→ {}({})\n", c.name, kk_llm::truncate(&c.arguments, 400)));
                }
            }
            EventKind::ToolResult { name, output, is_error, .. } => {
                let tag = if *is_error { "error" } else { "result" };
                s.push_str(&format!("← {name} {tag}: {}\n\n", kk_llm::truncate(output, max_tool_chars)));
            }
            _ => {}
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: i64, kind: EventKind) -> Event {
        Event { seq, ts: now(), agent_id: Some("a".into()), visibility: Visibility::Public, kind }
    }
    fn msg(seq: i64, t: &str) -> Event {
        ev(seq, EventKind::Message { to: "a".into(), from: Sender::User, text: t.into(), kind: MessageKind::Chat })
    }
    fn asst(seq: i64, calls: Vec<ToolCall>) -> Event {
        ev(seq, EventKind::Assistant { content: "ok".into(), reasoning: None, tool_calls: calls, usage: None, engine: "e".into() })
    }
    fn call(id: &str) -> ToolCall {
        ToolCall { id: id.into(), name: "read_file".into(), arguments: "{}".into() }
    }

    #[test]
    fn interrupted_calls_get_results_and_users_merge() {
        let h = History {
            summary: None,
            events: vec![
                msg(1, "hi"),
                msg(2, "also"),
                asst(3, vec![call("c1"), call("c2")]),
                ev(4, EventKind::ToolResult { call_id: "c1".into(), name: "read_file".into(), output: "x".into(), is_error: false }),
                ev(5, EventKind::ToolResult { call_id: "zz".into(), name: "orphan".into(), output: "x".into(), is_error: false }),
            ],
        };
        let m = to_messages(&h, &[], false);
        let roles: Vec<&str> = m.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant", "tool", "tool"]);
        assert_eq!(m[0].content.as_deref(), Some("hi\n\nalso"));
        assert_eq!(m[3].content.as_deref(), Some(INTERRUPTED));
    }

    #[test]
    fn text_mode_has_no_tool_role() {
        let h = History {
            summary: Some("earlier".into()),
            events: vec![
                asst(3, vec![call("c1")]),
                ev(4, EventKind::ToolResult { call_id: "c1".into(), name: "read_file".into(), output: "x".into(), is_error: false }),
            ],
        };
        let m = to_messages(&h, &[], true);
        assert!(m.iter().all(|m| m.role != "tool"));
        assert_eq!(m[0].role, "user");
        assert!(m[1].content.as_ref().unwrap().contains("<tool_call>"));
    }

    #[test]
    fn cut_lands_on_message() {
        let big = "x".repeat(1000);
        let events = vec![msg(1, &big), asst(2, vec![]), msg(3, &big), asst(4, vec![]), msg(5, "small"), asst(6, vec![])];
        let i = choose_cut(&events, 1500).unwrap();
        assert!(matches!(events[i].kind, EventKind::Message { .. }));
        assert!(i >= 2);
    }
}
