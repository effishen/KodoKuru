//! End-to-end tests of the agent loop against a scripted mock engine.

use axum::{extract::State, routing::post, Json, Router};
use kk_agent::Runtime;
use kk_core::*;
use kk_home::vault::{KdfParams, UnlockedVault};
use kk_home::Home;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Default)]
struct Script {
    /// agent display name -> queued responses
    turns: Arc<Mutex<HashMap<String, VecDeque<Value>>>>,
    /// every request body received
    log: Arc<Mutex<Vec<Value>>>,
}

fn tool_call(name: &str, args: Value) -> Value {
    json!({"choices": [{"message": {"content": "", "tool_calls": [
        {"id": format!("call_{}", rand_id()), "type": "function", "function": {"name": name, "arguments": args.to_string()}}
    ]}, "finish_reason": "tool_calls"}]})
}

fn text(t: &str) -> Value {
    json!({"choices": [{"message": {"content": t}, "finish_reason": "stop"}]})
}

fn rand_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::SeqCst)
}

async fn handler(State(s): State<Script>, Json(body): Json<Value>) -> Json<Value> {
    s.log.lock().unwrap().push(body.clone());
    let msgs = body["messages"].as_array().unwrap();
    let last = msgs.last().unwrap()["content"].as_str().unwrap_or("");
    if last.contains("KodoKuru completion check") {
        return Json(text(r#"{"status": "COMPLETE", "unmet_requirement": "", "next_action": ""}"#));
    }
    let system = msgs[0]["content"].as_str().unwrap_or("");
    let mut turns = s.turns.lock().unwrap();
    for (name, q) in turns.iter_mut() {
        if system.contains(&format!("You are **{name}**")) {
            return Json(q.pop_front().unwrap_or_else(|| text("(script exhausted)")));
        }
    }
    Json(text("(unknown agent)"))
}

async fn setup(script: Script) -> (tempfile::TempDir, Arc<Runtime>) {
    let app = Router::new().route("/v1/chat/completions", post(handler)).with_state(script);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

    let d = tempfile::tempdir().unwrap();
    let home = Home::open(&d.path().join("home"), &d.path().join("cache")).unwrap();
    home.save_engines_raw(&format!(
        "default_chat = \"mock\"\n[[engine]]\nid = \"mock\"\nbase_url = \"http://{addr}/v1\"\nmodel = \"m\"\napi_key = \"secret:mock\"\n"
    ))
    .unwrap();
    let (mut v, _) = UnlockedVault::create(&home.vault_path(), "password123", KdfParams::TEST).unwrap();
    v.set("mock", "sk-test").unwrap();
    let rt = Runtime::new(home).unwrap();
    rt.unlock(v).await;
    (d, rt)
}

fn finals(ws: &kk_agent::Workspace, agent: &str) -> Vec<String> {
    ws.store()
        .agent_events(agent, 0)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Assistant { content, tool_calls, .. } if tool_calls.is_empty() => Some(content),
            _ => None,
        })
        .collect()
}

async fn wait_for(mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for condition");
}

#[tokio::test]
async fn single_agent_checklist_nudge_and_files() {
    let script = Script::default();
    script.turns.lock().unwrap().insert(
        "Lead".into(),
        VecDeque::from(vec![
            tool_call("update_checklist", json!({"items": [
                {"content": "write hello.txt", "status": "in_progress"},
                {"content": "verify it", "status": "pending"}]})),
            tool_call("write_file", json!({"path": "hello.txt", "content": "hi there"})),
            text("Done."),
            tool_call("read_file", json!({"path": "hello.txt"})),
            tool_call("update_checklist", json!({"items": [
                {"content": "write hello.txt", "status": "completed"},
                {"content": "verify it", "status": "completed"}]})),
            text("hello.txt written and verified."),
        ]),
    );
    let (_d, rt) = setup(script.clone()).await;
    let ws = rt.create_workspace("Test", "general").await.unwrap();
    let lead = ws.lead().unwrap();
    ws.send_message(&lead.id, Sender::User, "write hello.txt", MessageKind::Chat).unwrap();
    wait_for(|| finals(&ws, &lead.id).len() >= 2 && !ws.is_running(&lead.id)).await;

    assert_eq!(std::fs::read_to_string(ws.files_dir().join("hello.txt")).unwrap(), "hi there");
    let events = ws.store().agent_events(&lead.id, 0).unwrap();
    // The harness nudged about the open checklist item.
    assert!(events.iter().any(|e| matches!(&e.kind,
        EventKind::Message { from: Sender::Harness, text, .. } if text.contains("open item"))));
    // The read_file result came back with line numbers.
    assert!(events.iter().any(|e| matches!(&e.kind,
        EventKind::ToolResult { name, output, .. } if name == "read_file" && output.contains("1│ hi there"))));
    assert_eq!(ws.store().run_state(&lead.id).unwrap(), Some(RunState::Idle));
    // The API key from the vault was sent; tools were offered.
    let log = script.log.lock().unwrap();
    assert!(log[0]["tools"].as_array().unwrap().iter().any(|t| t["function"]["name"] == "write_file"));
}

#[tokio::test]
async fn lead_delegates_and_receives_report() {
    let script = Script::default();
    {
        let mut t = script.turns.lock().unwrap();
        t.insert(
            "Lead".into(),
            VecDeque::from(vec![
                tool_call("spawn_agent", json!({"role": "developer"})),
                tool_call("assign_task", json!({
                    "agent": "Developer", "title": "Write answer",
                    "description": "Write 42 to out.txt", "acceptance_criteria": "out.txt contains 42"})),
                text("Assigned to Developer; waiting for the report."),
                text("All done: out.txt contains 42."),
            ]),
        );
        t.insert(
            "Developer".into(),
            VecDeque::from(vec![
                tool_call("write_file", json!({"path": "out.txt", "content": "42"})),
                tool_call("board_update", json!({"task_id": "T-1", "status": "review", "note": "written"})),
                text("Wrote out.txt with 42; read it back."),
            ]),
        );
    }
    let (_d, rt) = setup(script.clone()).await;
    let ws = rt.create_workspace("Team", "general").await.unwrap();
    let lead = ws.lead().unwrap();
    ws.send_message(&lead.id, Sender::User, "get the answer into out.txt", MessageKind::Chat).unwrap();
    wait_for(|| finals(&ws, &lead.id).iter().any(|f| f.contains("All done")) && !ws.is_running(&lead.id)).await;

    let dev = ws.find_agent("Developer").unwrap();
    assert!(!dev.is_lead);
    assert_eq!(std::fs::read_to_string(ws.files_dir().join("out.txt")).unwrap(), "42");
    // The developer's final reply was delivered to the lead as a report.
    let lead_events = ws.store().agent_events(&lead.id, 0).unwrap();
    assert!(lead_events.iter().any(|e| matches!(&e.kind,
        EventKind::Message { from: Sender::Agent { id }, kind: MessageKind::Report, text, .. }
            if id == &dev.id && text.contains("Wrote out.txt"))));
    // The task is on the board, in review.
    let t = ws.store().task("T-1").unwrap().unwrap();
    assert_eq!(t.status, TaskStatus::Review);
    assert_eq!(t.assignee.as_deref(), Some(dev.id.as_str()));
    // The lead's report-handling turn didn't bounce a report back to the developer.
    let dev_reports = ws.store().agent_events(&dev.id, 0).unwrap().into_iter()
        .filter(|e| matches!(&e.kind, EventKind::Message { kind: MessageKind::Report, .. })).count();
    assert_eq!(dev_reports, 0);
}

#[tokio::test]
async fn resume_marks_interrupted_tool_calls() {
    let script = Script::default();
    script.turns.lock().unwrap().insert("Lead".into(), VecDeque::from(vec![text("Checked state; resuming is fine.")]));
    let (_d, rt) = setup(script.clone()).await;
    let ws = rt.create_workspace("Resume", "general").await.unwrap();
    let lead = ws.lead().unwrap();
    // Simulate a crash mid-tool-call: assistant asked for a tool, no result, state Running.
    ws.emit(Some(&lead.id), EventKind::Message { to: lead.id.clone(), from: Sender::User, text: "go".into(), kind: MessageKind::Chat }).unwrap();
    ws.emit(Some(&lead.id), EventKind::Assistant {
        content: String::new(), reasoning: None, engine: "mock".into(), usage: None,
        tool_calls: vec![ToolCall { id: "c9".into(), name: "write_file".into(), arguments: "{}".into() }],
    }).unwrap();
    ws.set_run_state(&lead.id, RunState::Running);
    ws.resume().await;
    wait_for(|| !finals(&ws, &lead.id).is_empty() && !ws.is_running(&lead.id)).await;
    let log = script.log.lock().unwrap();
    let tool_msg = log[0]["messages"].as_array().unwrap().iter().find(|m| m["role"] == "tool").unwrap().clone();
    assert!(tool_msg["content"].as_str().unwrap().contains("interrupted"));
}

#[tokio::test]
async fn locked_runtime_does_not_run() {
    let script = Script::default();
    script.turns.lock().unwrap().insert("Lead".into(), VecDeque::from(vec![text("hello")]));
    let (_d, rt) = setup(script.clone()).await;
    let ws = rt.create_workspace("Locked", "general").await.unwrap();
    let lead = ws.lead().unwrap();
    rt.lock().await;
    ws.send_message(&lead.id, Sender::User, "hi", MessageKind::Chat).unwrap();
    wait_for(|| ws.store().run_state(&lead.id).unwrap() == Some(RunState::Locked)).await;
    assert!(script.log.lock().unwrap().is_empty());
}
