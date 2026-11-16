import { useCallback, useEffect, useMemo, useState } from "react";
import { api, live } from "./api";
import { useGlobalLive, useLocalStorage, useWorkspace } from "./hooks";
import type { AgentView, AppState, Template, WorkspaceSummary } from "./types";
import { Login, Setup } from "./components/Auth";
import { Chat, stateLabel } from "./components/Chat";
import { Activity, Close, Gear, LockIcon, Logo, Panel, Plus } from "./components/Icons";
import { Board, ChecklistView, Envs, Files } from "./components/SidePanel";
import { Settings } from "./components/Settings";

function StatusDot({ v }: { v: AgentView }) {
  const s = v.run_state.state;
  const cls = v.running ? (s === "waiting_engine" ? "waiting" : s === "waiting_approval" ? "approval" : "running") : s === "error" ? "error" : s === "paused" || s === "stopped" ? "paused" : "idle";
  return <span className={`status-dot ${cls}`} title={stateLabel(v.run_state)} />;
}

function NewWorkspace({ onClose, onCreated }: { onClose: () => void; onCreated: (id: string) => void }) {
  const [templates, setTemplates] = useState<Template[]>([]);
  const [tpl, setTpl] = useState("general");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    api.get<{ templates: { template: Template }[] }>("/templates").then((r) => setTemplates(r.templates.map((t) => t.template)));
  }, []);
  const create = async () => {
    setBusy(true);
    try {
      const r = await api.post<{ id: string }>("/workspaces", { name, template: tpl });
      onCreated(r.id);
    } catch (e: any) {
      setErr(e.message);
      setBusy(false);
    }
  };
  return (
    <div className="modal-bg" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h2>New workspace</h2>
          <button className="icon-btn" onClick={onClose}>
            <Close />
          </button>
        </div>
        <p className="muted small">Each workspace is an isolated folder with its own agents, files, environments and history. Copy the folder to move it to another machine.</p>
        <label className="form">
          Name
          <input autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Router firmware analysis" onKeyDown={(e) => e.key === "Enter" && name && create()} />
        </label>
        <div className="templates">
          {templates.map((t) => (
            <button key={t.id} className={`template ${tpl === t.id ? "on" : ""}`} onClick={() => setTpl(t.id)}>
              <b>{t.name}</b>
              <span className="muted small">{t.description}</span>
              <span className="small">
                Lead: {t.lead_role}
                {t.initial_agents.length ? ` · team: ${t.initial_agents.join(", ")}` : ""}
              </span>
            </button>
          ))}
        </div>
        {err && <div className="error">{err}</div>}
        <div className="row-actions">
          <button className="btn primary" disabled={!name.trim() || busy} onClick={create}>
            {busy ? "Creating…" : "Create workspace"}
          </button>
        </div>
      </div>
    </div>
  );
}

type PanelTab = "checklist" | "board" | "envs" | "files";

function Main({ app, onLocked }: { app: AppState; onLocked: () => void }) {
  const [workspaces, setWorkspaces] = useState<WorkspaceSummary[]>([]);
  const [wsId, setWsId] = useLocalStorage<string | null>("kk.ws", null);
  const [agentSel, setAgentSel] = useLocalStorage<Record<string, string>>("kk.agent", {});
  const [panel, setPanel] = useLocalStorage<boolean>("kk.panel", true);
  const [tab, setTab] = useLocalStorage<PanelTab>("kk.tab", "checklist");
  const [showInternal, setShowInternal] = useLocalStorage<boolean>("kk.internal", true);
  const [creating, setCreating] = useState(false);
  const [settings, setSettings] = useState(false);

  const loadWs = useCallback(() => api.get<{ workspaces: WorkspaceSummary[] }>("/workspaces").then((r) => setWorkspaces(r.workspaces)), []);
  useEffect(() => {
    loadWs();
  }, [loadWs]);
  const { health, connected } = useGlobalLive(onLocked, loadWs);
  useEffect(() => {
    if (workspaces.length && (!wsId || !workspaces.some((w) => w.id === wsId))) setWsId(workspaces[0].id);
  }, [workspaces, wsId, setWsId]);

  const { detail, events, streams, envLogs } = useWorkspace(wsId);
  const lead = detail?.agents.find((a) => a.agent.is_lead);
  const sel = (wsId && agentSel[wsId]) || lead?.agent.id || "all";
  const agentId = sel === "all" || detail?.agents.some((a) => a.agent.id === sel) ? sel : lead?.agent.id ?? "all";
  const current = detail?.agents.find((a) => a.agent.id === agentId);
  const fileVersion = useMemo(() => events.filter((e) => e.type === "tool_result").length, [events]);

  useEffect(() => {
    // Keep sidebar counts fresh while agents run.
    const t = window.setInterval(loadWs, 15000);
    return () => window.clearInterval(t);
  }, [loadWs]);

  const select = (id: string) => wsId && setAgentSel({ ...agentSel, [wsId]: id });
  const logout = async () => {
    await api.post("/logout");
    location.reload();
  };
  // Agents can't think without their chat engine; other kinds show in the footer chips.
  const offline = health.filter((h) => h.status === "offline" && h.kind === "chat");

  return (
    <div className={`app ${panel && detail && !settings ? "with-panel" : ""}`}>
      <aside className="sidebar">
        <div className="brand">
          <Logo /> <span>KodoKuru</span>
        </div>
        <button className="new-ws" onClick={() => setCreating(true)}>
          <Plus size={15} /> New workspace
        </button>
        <div className="side-scroll">
          <div className="side-label">Workspaces</div>
          {workspaces.map((w) => (
            <div key={w.id}>
              <button className={`ws-item ${w.id === wsId ? "on" : ""}`} onClick={() => (setWsId(w.id), setSettings(false))}>
                <span className="ws-name">{w.name}</span>
                {w.approvals > 0 && <span className="badge warn">{w.approvals}</span>}
                {w.running > 0 && <span className="badge">{w.running}</span>}
              </button>
              {w.id === wsId && detail && (
                <div className="agents">
                  <button className={`agent-item ${agentId === "all" ? "on" : ""}`} onClick={() => (select("all"), setSettings(false))}>
                    <Activity size={13} /> <span>All activity</span>
                  </button>
                  {detail.agents.map((a) => (
                    <button key={a.agent.id} className={`agent-item ${agentId === a.agent.id ? "on" : ""}`} onClick={() => (select(a.agent.id), setSettings(false))}>
                      <StatusDot v={a} />
                      <span>{a.agent.name}</span>
                      {a.agent.is_lead && <span className="muted small">lead</span>}
                    </button>
                  ))}
                </div>
              )}
            </div>
          ))}
          {!workspaces.length && <p className="muted small pad">No workspaces yet.</p>}
        </div>
        <div className="side-foot">
          <div className="engines-mini">
            {health.map((h) => (
              <span key={h.id} className="engine-chip" title={`${h.id}: ${h.status}${h.detail ? " — " + h.detail : ""}`}>
                <span className={`dot-s ${h.status}`} /> {h.id}
                {h.busy > 0 && <span className="muted"> {h.busy}/{h.capacity}</span>}
              </span>
            ))}
            {!health.length && <span className="muted small">No engines configured</span>}
          </div>
          <div className="foot-actions">
            <button className="icon-btn" title="Settings" onClick={() => setSettings(true)}>
              <Gear />
            </button>
            <button className="icon-btn" title="Lock" onClick={() => api.post("/lock").then(() => location.reload())}>
              <LockIcon />
            </button>
            <button className="link small" onClick={logout}>
              Log out
            </button>
            {!connected && <span className="muted small">reconnecting…</span>}
          </div>
        </div>
      </aside>

      <main className="main">
        {settings ? (
          <Settings app={app} health={health} wsId={wsId} onClose={() => setSettings(false)} />
        ) : !detail ? (
          <div className="empty-main">
            <Logo size={48} />
            <h2>{workspaces.length ? "Loading…" : "Create your first workspace"}</h2>
            {!workspaces.length && (
              <button className="btn primary" onClick={() => setCreating(true)}>
                New workspace
              </button>
            )}
          </div>
        ) : (
          <>
            <header className="topbar">
              <div className="topbar-title">
                <b>{agentId === "all" ? "All activity" : current?.agent.name}</b>
                {current &&
                  (() => {
                    const role = detail.roles.find((r) => r.id === current.agent.role_id)?.name ?? current.agent.role_id;
                    return role !== current.agent.name ? <span className="muted"> · {role}</span> : null;
                  })()}
                <span className="muted"> · {detail.manifest.name}</span>
              </div>
              <div className="topbar-right">
                {current && <span className={`state-text ${current.run_state.state}`}>{stateLabel(current.run_state)}</span>}
                <label className="toggle small" title="Show harness messages, completion checks and internal team traffic">
                  <input type="checkbox" checked={showInternal} onChange={(e) => setShowInternal(e.target.checked)} /> details
                </label>
                <button className={`icon-btn ${panel ? "on" : ""}`} title="Side panel" onClick={() => setPanel(!panel)}>
                  <Panel />
                </button>
              </div>
            </header>
            {offline.length > 0 && (
              <div className="banner warn">
                Engine{offline.length > 1 ? "s" : ""} offline: {offline.map((o) => o.id).join(", ")}. Agents wait and retry automatically.
              </div>
            )}
            {detail.approvals.length > 0 && agentId !== "all" && !detail.approvals.some((a) => a.agent_id === agentId) && (
              <div className="banner info">
                {detail.approvals.length} approval{detail.approvals.length > 1 ? "s" : ""} waiting:{" "}
                {detail.approvals.map((a) => (
                  <button key={a.approval_id} className="link" onClick={() => select(a.agent_id)}>
                    {detail.agents.find((x) => x.agent.id === a.agent_id)?.agent.name}
                  </button>
                ))}
              </div>
            )}
            <Chat ws={detail} events={events} streams={streams} agentId={agentId} showInternal={showInternal} />
          </>
        )}
      </main>

      {panel && detail && !settings && (
        <aside className="panel">
          <div className="tabs">
            {(["checklist", "board", "envs", "files"] as PanelTab[]).map((t) => (
              <button key={t} className={tab === t ? "on" : ""} onClick={() => setTab(t)}>
                {t === "envs" ? "Environments" : t[0].toUpperCase() + t.slice(1)}
              </button>
            ))}
          </div>
          <div className="panel-body">
            {tab === "checklist" &&
              (current ? (
                <ChecklistView items={current.checklist} />
              ) : (
                detail.agents.map((a) => (
                  <div key={a.agent.id} className="panel-section">
                    <div className="panel-sub">{a.agent.name}</div>
                    <ChecklistView items={a.checklist} />
                  </div>
                ))
              ))}
            {tab === "board" && <Board tasks={detail.tasks} agents={detail.agents.map((a) => a.agent)} />}
            {tab === "envs" && <Envs ws={detail} logs={envLogs} />}
            {tab === "files" && <Files wsId={detail.manifest.id} version={fileVersion} />}
          </div>
        </aside>
      )}

      {creating && (
        <NewWorkspace
          onClose={() => setCreating(false)}
          onCreated={(id) => {
            setCreating(false);
            loadWs().then(() => setWsId(id));
          }}
        />
      )}
    </div>
  );
}

export function App() {
  const [state, setState] = useState<AppState | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const load = useCallback(() => {
    api
      .get<AppState>("/state")
      .then((s) => {
        setState(s);
        setErr(null);
        if (s.authenticated) live.start();
      })
      .catch((e) => setErr(e.message));
  }, []);
  useEffect(() => {
    load();
    const h = () => {
      live.stop();
      load();
    };
    window.addEventListener("kk-unauthorized", h);
    return () => window.removeEventListener("kk-unauthorized", h);
  }, [load]);
  const onLocked = useCallback(() => {
    live.stop();
    load();
  }, [load]);

  if (err) return <div className="empty-main">Cannot reach KodoKuru: {err}</div>;
  if (!state) return null;
  if (state.setup_required) return <Setup onDone={load} />;
  if (!state.authenticated) return <Login locked={state.locked} onDone={load} />;
  return <Main app={state} onLocked={onLocked} />;
}
