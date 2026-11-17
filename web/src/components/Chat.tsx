import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import type { Stream } from "../hooks";
import type { AgentInfo, AgentView, KEvent, RunState, ToolCall, WorkspaceDetail } from "../types";
import { Brain, Chevron, Send, StopIcon, Tool, Users } from "./Icons";
import { Markdown } from "./Markdown";

type ToolResultEv = Extract<KEvent, { type: "tool_result" }>;

export function stateLabel(s: RunState | undefined): string {
  switch (s?.state) {
    case "running":
      return "Working";
    case "waiting_engine":
      return `Waiting for ${s.engine}: ${s.reason}`;
    case "waiting_approval":
      return "Waiting for your approval";
    case "locked":
      return "Locked — log in to continue";
    case "stopped":
      return "Stopped";
    case "paused":
      return `Paused: ${s.reason}`;
    case "error":
      return `Error: ${s.message}`;
    default:
      return "Idle";
  }
}

function parseArgs(a: string): Record<string, any> {
  try {
    const v = JSON.parse(a);
    return v && typeof v === "object" ? v : {};
  } catch {
    return {};
  }
}

function toolSummary(c: ToolCall): string {
  const a = parseArgs(c.arguments);
  switch (c.name) {
    case "run_command":
      return a.command ?? "";
    case "read_file":
    case "write_file":
    case "edit_file":
    case "delete_path":
      return a.path ?? "";
    case "list_files":
      return a.path ?? ".";
    case "search_text":
      return `/${a.pattern ?? ""}/ ${a.path ?? ""}`;
    case "move_path":
      return `${a.from} → ${a.to}`;
    case "update_checklist":
      return `${(a.items ?? []).length} items`;
    case "assign_task":
      return `${a.agent}: ${a.title ?? ""}`;
    case "message_agent":
      return `${a.agent}`;
    case "spawn_agent":
      return a.name ? `${a.role} (${a.name})` : a.role;
    case "define_role":
      return a.id ?? "";
    case "board_update":
      return `${a.task_id} ${a.status ?? ""}`;
    case "state_set":
      return `${a.key} = ${JSON.stringify(a.value)}`.slice(0, 80);
    case "roll_dice":
      return a.dice ?? "";
    default: {
      const first = Object.values(a).find((v) => typeof v === "string") as string | undefined;
      return first ? first.slice(0, 80) : "";
    }
  }
}

const TOOL_LABEL: Record<string, string> = {
  run_command: "Ran",
  read_file: "Read",
  write_file: "Wrote",
  edit_file: "Edited",
  list_files: "Listed",
  search_text: "Searched",
  update_checklist: "Updated checklist",
  assign_task: "Assigned task",
  message_agent: "Messaged",
  spawn_agent: "Spawned agent",
  define_role: "Defined role",
  board_update: "Updated board",
  board_create: "Added task",
  board_list: "Checked board",
  list_agents: "Listed agents",
  list_engines: "Listed engines",
  list_roles: "Listed roles",
  read_transcript: "Read transcript",
  env_start: "Started environment",
  text_to_speech: "Generated speech",
  generate_image: "Generated image",
  state_get: "Read state",
  state_set: "Set state",
  roll_dice: "Rolled",
};

function ToolRow({ call, result, pending }: { call: ToolCall; result?: ToolResultEv; pending: boolean }) {
  const [open, setOpen] = useState(false);
  const [full, setFull] = useState(false);
  const status = result ? (result.is_error ? "error" : "ok") : pending ? "pending" : "lost";
  const out = result?.output ?? "";
  const shown = full || out.length < 6000 ? out : out.slice(0, 6000) + "\n…";
  return (
    <div className={`tool ${status}`}>
      <button className="tool-head" onClick={() => setOpen(!open)}>
        <Tool size={14} />
        <span className="tool-name">{TOOL_LABEL[call.name] ?? call.name}</span>
        <span className="tool-sum">{toolSummary(call)}</span>
        <span className={`tool-status ${status}`}>{status === "pending" ? <span className="spinner" /> : status === "error" ? "failed" : ""}</span>
        <Chevron size={14} open={open} />
      </button>
      {open && (
        <div className="tool-body">
          <div className="tool-label">{call.name}</div>
          <ArgsView raw={call.arguments} />
          {result && (
            <>
              <div className="tool-label">{result.is_error ? "Error" : "Result"}</div>
              <pre className="tool-out">{shown}</pre>
              {out.length >= 6000 && !full && (
                <button className="link" onClick={() => setFull(true)}>
                  Show all ({out.length.toLocaleString()} characters)
                </button>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

/** Arguments with multi-line strings (file contents, scripts) shown as text. */
function ArgsView({ raw }: { raw: string }) {
  const a = parseArgs(raw);
  const keys = Object.keys(a);
  if (!keys.length) return <pre>{raw || "{}"}</pre>;
  return (
    <div className="args">
      {keys.map((k) => {
        const v = a[k];
        const multi = typeof v === "string" && (v.includes("\n") || v.length > 120);
        return (
          <div key={k} className={multi ? "arg multi" : "arg"}>
            <span className="arg-k">{k}</span>
            {multi ? <pre>{v}</pre> : <code>{typeof v === "string" ? v : JSON.stringify(v)}</code>}
          </div>
        );
      })}
    </div>
  );
}

function Thinking({ text, live }: { text: string; live?: boolean }) {
  const [open, setOpen] = useState(false);
  if (!text.trim()) return null;
  return (
    <div className="thinking">
      <button className="thinking-head" onClick={() => setOpen(!open)}>
        <Brain size={14} />
        <span>{live ? "Thinking…" : "Thought process"}</span>
        <Chevron size={14} open={open} />
      </button>
      {open && <div className="thinking-body">{text}</div>}
    </div>
  );
}

function Collapsible({ title, children, className }: { title: React.ReactNode; children: React.ReactNode; className?: string }) {
  const [open, setOpen] = useState(false);
  return (
    <div className={className}>
      <button className="collapse-head" onClick={() => setOpen(!open)}>
        {title} <Chevron size={13} open={open} />
      </button>
      {open && <div className="collapse-body">{children}</div>}
    </div>
  );
}

function ApprovalCard({ ev, pending, wsId, resolved }: { ev: Extract<KEvent, { type: "approval_requested" }>; pending: boolean; wsId: string; resolved?: Extract<KEvent, { type: "approval_resolved" }> }) {
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const act = async (approved: boolean) => {
    setBusy(true);
    try {
      await api.post(`/workspaces/${wsId}/approvals/${ev.approval_id}`, { approved, note });
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className={`approval ${pending ? "pending" : ""}`}>
      <div className="approval-title">{pending ? "Approval needed" : resolved?.approved ? "Approved" : "Not approved"}</div>
      <div className="approval-summary">{ev.summary}</div>
      {pending && (
        <div className="approval-actions">
          <input placeholder="Optional note to the agent" value={note} onChange={(e) => setNote(e.target.value)} />
          <button className="btn primary" disabled={busy} onClick={() => act(true)}>
            Approve
          </button>
          <button className="btn ghost" disabled={busy} onClick={() => act(false)}>
            Deny
          </button>
        </div>
      )}
      {resolved?.note && <div className="muted small">Note: {resolved.note}</div>}
    </div>
  );
}

interface TimelineProps {
  ws: WorkspaceDetail;
  events: KEvent[];
  agentId: string | "all";
  stream?: Stream;
  streams: Record<string, Stream>;
  agents: AgentInfo[];
  showInternal: boolean;
}

function Timeline({ ws, events, agentId, streams, agents, showInternal }: TimelineProps) {
  const name = (id: string) => agents.find((a) => a.id === id)?.name ?? id;
  const results = useMemo(() => {
    const m = new Map<string, ToolResultEv>();
    for (const e of events) if (e.type === "tool_result") m.set(e.call_id, e);
    return m;
  }, [events]);
  const resolutions = useMemo(() => {
    const m = new Map<string, Extract<KEvent, { type: "approval_resolved" }>>();
    for (const e of events) if (e.type === "approval_resolved") m.set(e.approval_id, e);
    return m;
  }, [events]);
  const pendingApprovals = new Set(ws.approvals.map((a) => a.approval_id));
  const runningAgents = new Set(ws.agents.filter((a) => a.running).map((a) => a.agent.id));

  const shown = events.filter(
    (e) =>
      (agentId === "all" ? e.agent_id !== null || e.type === "notice" : e.agent_id === agentId) &&
      (showInternal || e.visibility === "public" || e.type === "message"),
  );
  const items: React.ReactNode[] = [];
  for (const e of shown) {
    const who = e.agent_id && agentId === "all" ? <div className="who">{name(e.agent_id)}</div> : null;
    switch (e.type) {
      case "message": {
        if (e.from.kind === "user") {
          items.push(
            <div key={e.seq} className="row user">
              {agentId === "all" && <div className="who right">You → {name(e.to)}</div>}
              <div className="bubble">{e.text}</div>
            </div>,
          );
        } else if (e.from.kind === "harness" || e.kind.kind === "harness") {
          if (!showInternal) break;
          items.push(
            <Collapsible key={e.seq} className="harness" title={<span>KodoKuru · {e.text.split("\n")[0].slice(0, 90)}</span>}>
              <pre className="plain">{e.text}</pre>
            </Collapsible>,
          );
        } else {
          const from = e.from.kind === "agent" ? name(e.from.id) : "?";
          const label =
            e.kind.kind === "task_assignment"
              ? `Task ${e.kind.task_id} from ${from}`
              : e.kind.kind === "report"
                ? `Report from ${from}`
                : `Message from ${from}`;
          items.push(
            <div key={e.seq} className={`row agent-msg ${e.kind.kind}`}>
              <div className="agent-msg-head">
                <Users size={14} /> {label}
                {agentId === "all" && <span className="muted"> → {name(e.to)}</span>}
              </div>
              <Markdown text={e.text} />
            </div>,
          );
        }
        break;
      }
      case "assistant": {
        const pending = !!e.agent_id && runningAgents.has(e.agent_id);
        items.push(
          <div key={e.seq} className="row assistant">
            {who}
            {e.reasoning && <Thinking text={e.reasoning} />}
            {e.content.trim() && <Markdown text={e.content} />}
            {e.tool_calls.length > 0 && (
              <div className="tools">
                {e.tool_calls.map((c) => (
                  <ToolRow key={c.id} call={c} result={results.get(c.id)} pending={pending} />
                ))}
              </div>
            )}
          </div>,
        );
        break;
      }
      case "approval_requested":
        items.push(<ApprovalCard key={e.seq} ev={e} wsId={ws.manifest.id} pending={pendingApprovals.has(e.approval_id)} resolved={resolutions.get(e.approval_id)} />);
        break;
      case "compaction":
        items.push(
          <Collapsible key={e.seq} className="divider-c" title={<span>Earlier context was summarised</span>}>
            <Markdown text={e.summary} />
          </Collapsible>,
        );
        break;
      case "notice":
        if (e.level === "check" && !showInternal) break;
        items.push(
          <div key={e.seq} className={`notice ${e.level}`}>
            {agentId === "all" && e.agent_id ? `${name(e.agent_id)}: ` : ""}
            {e.text}
          </div>,
        );
        break;
      case "agent_created":
        if (agentId === "all")
          items.push(
            <div key={e.seq} className="notice">
              {e.agent.name} joined ({e.agent.role_id})
            </div>,
          );
        break;
    }
  }
  const liveAgents = agentId === "all" ? Object.keys(streams) : streams[agentId] ? [agentId] : [];
  for (const a of liveAgents) {
    const s = streams[a];
    items.push(
      <div key={`live-${a}`} className="row assistant live">
        {agentId === "all" && <div className="who">{name(a)}</div>}
        <Thinking text={s.reasoning} live={!s.text} />
        {s.text && <Markdown text={s.text + " ▍"} />}
      </div>,
    );
  }
  return <>{items}</>;
}

export function Chat({
  ws,
  events,
  streams,
  agentId,
  showInternal,
}: {
  ws: WorkspaceDetail;
  events: KEvent[];
  streams: Record<string, Stream>;
  agentId: string | "all";
  showInternal: boolean;
}) {
  const agents = ws.agents.map((a) => a.agent);
  const view: AgentView | undefined = ws.agents.find((a) => a.agent.id === agentId);
  const lead = ws.agents.find((a) => a.agent.is_lead);
  const target = view ?? lead;
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);

  const onScroll = () => {
    const el = scroller.current;
    if (el) stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
  };
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });
  useEffect(() => {
    stick.current = true;
  }, [agentId]);

  const send = async () => {
    if (!text.trim() || !target) return;
    setSending(true);
    setErr(null);
    try {
      await api.post(`/workspaces/${ws.manifest.id}/messages`, { agent_id: target.agent.id, text });
      setText("");
      stick.current = true;
    } catch (e: any) {
      setErr(e.message);
    } finally {
      setSending(false);
    }
  };
  const stop = () => target && api.post(`/workspaces/${ws.manifest.id}/agents/${target.agent.id}/stop`);
  const running = target?.running;
  const st = target?.run_state;
  const empty = !events.some((e) => (agentId === "all" ? e.agent_id : e.agent_id === agentId) && (e.type === "message" || e.type === "assistant"));

  return (
    <div className="chat">
      <div className="chat-scroll" ref={scroller} onScroll={onScroll}>
        <div className="chat-inner">
          {empty && view?.agent.is_lead && (
            <div className="welcome">
              <h2>What should {ws.manifest.name} work on?</h2>
              <p className="muted">
                You're talking to <b>{view.agent.name}</b>, the lead. It plans the work, builds a team when needed, and reports back. Everything the agents do appears here and in their own chats.
              </p>
            </div>
          )}
          {empty && view && !view.agent.is_lead && (
            <div className="welcome">
              <h2>{view.agent.name}</h2>
              <p className="muted">No activity yet. Work arrives here when the lead assigns it.</p>
            </div>
          )}
          <Timeline ws={ws} events={events} agentId={agentId} streams={streams} agents={agents} showInternal={showInternal} />
          {agentId !== "all" && running && !streams[agentId] && st?.state !== "waiting_approval" && (
            <div className={`working ${st?.state}`}>
              <span className="spinner" /> {stateLabel(st)}…
            </div>
          )}
          {agentId !== "all" && !running && (st?.state === "error" || st?.state === "paused" || st?.state === "locked") && (
            <div className={`notice ${st.state === "error" ? "error" : "warn"}`}>{stateLabel(st)}</div>
          )}
        </div>
      </div>
      {agentId !== "all" && target && (
        <div className="composer-wrap">
          {!target.agent.is_lead && <div className="step-in">You're messaging {target.agent.name} directly — normally the lead coordinates.</div>}
          {err && <div className="error">{err}</div>}
          <div className="composer">
            <textarea
              value={text}
              rows={1}
              placeholder={`Message ${target.agent.name}…`}
              onChange={(e) => {
                setText(e.target.value);
                e.target.style.height = "auto";
                e.target.style.height = Math.min(e.target.scrollHeight, 280) + "px";
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                  e.preventDefault();
                  send();
                }
              }}
            />
            <div className="composer-actions">
              {running && (
                <button className="icon-btn" title="Stop" onClick={stop}>
                  <StopIcon size={16} />
                </button>
              )}
              <button className="send" title="Send (Enter)" disabled={!text.trim() || sending} onClick={send}>
                <Send size={16} />
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
