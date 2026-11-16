import { useEffect, useState } from "react";
import { api } from "../api";
import type { AgentInfo, ChecklistItem, Task, TaskStatus, WorkspaceDetail } from "../types";
import { Check, FileIcon, Folder } from "./Icons";
import { Markdown } from "./Markdown";

export function ChecklistView({ items }: { items: ChecklistItem[] }) {
  if (!items.length) return <p className="muted small">No checklist yet. Agents write one when they plan a task.</p>;
  const done = items.filter((i) => i.status === "completed").length;
  return (
    <div>
      <div className="progress">
        <div style={{ width: `${(done / items.length) * 100}%` }} />
      </div>
      <div className="muted small">
        {done} of {items.length} done
      </div>
      <ul className="checklist">
        {items.map((i, n) => (
          <li key={n} className={i.status}>
            <span className="box">{i.status === "completed" ? <Check size={12} /> : i.status === "in_progress" ? <span className="dot" /> : null}</span>
            <span>{i.content}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

const COLUMNS: { id: TaskStatus; label: string }[] = [
  { id: "todo", label: "To do" },
  { id: "in_progress", label: "In progress" },
  { id: "review", label: "Review" },
  { id: "blocked", label: "Blocked" },
  { id: "done", label: "Done" },
];

export function Board({ tasks, agents }: { tasks: Task[]; agents: AgentInfo[] }) {
  const [open, setOpen] = useState<string | null>(null);
  const name = (id: string | null) => agents.find((a) => a.id === id)?.name ?? "unassigned";
  if (!tasks.length) return <p className="muted small">The board is empty. The lead adds tasks when it delegates work.</p>;
  return (
    <div className="board">
      {COLUMNS.map((c) => {
        const ts = tasks.filter((t) => t.status === c.id);
        if (!ts.length) return null;
        return (
          <div key={c.id} className="board-col">
            <div className="board-col-head">
              <span className={`pill ${c.id}`}>{c.label}</span> <span className="muted small">{ts.length}</span>
            </div>
            {ts.map((t) => (
              <div key={t.id} className="task" onClick={() => setOpen(open === t.id ? null : t.id)}>
                <div className="task-title">
                  <span className="muted">{t.id}</span> {t.title}
                </div>
                <div className="muted small">{name(t.assignee)}</div>
                {open === t.id && (
                  <div className="task-detail">
                    {t.description && <Markdown text={t.description} />}
                    {t.acceptance && (
                      <>
                        <div className="tool-label">Acceptance criteria</div>
                        <Markdown text={t.acceptance} />
                      </>
                    )}
                    {t.notes.map((n, i) => (
                      <div key={i} className="task-note">
                        <b>{n.by.kind === "agent" ? name(n.by.id) : n.by.kind === "user" ? "You" : "KodoKuru"}:</b> {n.text}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            ))}
          </div>
        );
      })}
    </div>
  );
}

export function Envs({ ws, logs }: { ws: WorkspaceDetail; logs: Record<string, string[]> }) {
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const act = async (name: string, action: string) => {
    if (action === "rebuild" && !confirm(`Delete the VM "${name}" so it is rebuilt from its recipe on next start? Changes made inside the VM (outside /workspace) are lost.`)) return;
    setBusy(name + action);
    setErr(null);
    try {
      await api.post(`/workspaces/${ws.manifest.id}/envs/${name}/${action}`);
    } catch (e: any) {
      setErr(e.message);
    } finally {
      setBusy(null);
    }
  };
  if (!ws.envs.length) return <p className="muted small">No environments. Agents only have file tools; the lead can define a VM with env_define.</p>;
  return (
    <div className="envs">
      {err && <div className="error">{err}</div>}
      {ws.envs.map(({ recipe, status }) => (
        <div key={recipe.name} className="env">
          <div className="env-head">
            <b>{recipe.name}</b> <span className="muted small">{recipe.kind}</span>
            <span className={`pill env-${status.state}`}>{status.state.replace("_", " ")}</span>
          </div>
          {recipe.description && <div className="muted small">{recipe.description}</div>}
          {recipe.kind === "lima" && (
            <div className="muted small">
              {recipe.cpus} CPU · {recipe.memory} · {recipe.disk} · disk {recipe.keep_disk ? "kept in workspace" : "in cache"}
            </div>
          )}
          {status.message && <div className="error small">{status.message}</div>}
          {recipe.kind === "lima" && (
            <div className="env-actions">
              <button className="btn small" disabled={!!busy} onClick={() => act(recipe.name, "start")}>
                Start
              </button>
              <button className="btn small ghost" disabled={!!busy} onClick={() => act(recipe.name, "stop")}>
                Stop
              </button>
              <button className="btn small ghost" disabled={!!busy} onClick={() => act(recipe.name, "rebuild")}>
                Rebuild
              </button>
            </div>
          )}
          {logs[recipe.name]?.length ? <pre className="env-log">{logs[recipe.name].slice(-40).join("\n")}</pre> : null}
        </div>
      ))}
    </div>
  );
}

interface Entry {
  name: string;
  dir: boolean;
  size: number;
}

export function Files({ wsId, version }: { wsId: string; version: number }) {
  const [path, setPath] = useState("");
  const [entries, setEntries] = useState<Entry[]>([]);
  const [file, setFile] = useState<{ path: string; content: string | null; size: number; binary: boolean } | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    api
      .get<{ entries: Entry[] }>(`/workspaces/${wsId}/files?path=${encodeURIComponent(path)}`)
      .then((r) => {
        setEntries(r.entries);
        setErr(null);
      })
      .catch((e) => setErr(e.message));
  }, [wsId, path, version]);
  const open = async (e: Entry) => {
    const p = path ? `${path}/${e.name}` : e.name;
    if (e.dir) {
      setPath(p);
      setFile(null);
      return;
    }
    const r = await api.get<any>(`/workspaces/${wsId}/files?path=${encodeURIComponent(p)}`);
    setFile({ path: p, content: r.content, size: r.size, binary: r.binary });
  };
  const crumbs = path ? path.split("/") : [];
  return (
    <div className="files">
      <div className="crumbs">
        <button className="link" onClick={() => (setPath(""), setFile(null))}>
          files
        </button>
        {crumbs.map((c, i) => (
          <span key={i}>
            {" / "}
            <button className="link" onClick={() => (setPath(crumbs.slice(0, i + 1).join("/")), setFile(null))}>
              {c}
            </button>
          </span>
        ))}
      </div>
      {err && <div className="error small">{err}</div>}
      <ul className="file-list">
        {entries.map((e) => (
          <li key={e.name} onClick={() => open(e)} className={file?.path.endsWith("/" + e.name) || file?.path === e.name ? "sel" : ""}>
            {e.dir ? <Folder size={14} /> : <FileIcon size={14} />} <span>{e.name}</span>
            {!e.dir && <span className="muted small">{fmtSize(e.size)}</span>}
          </li>
        ))}
        {!entries.length && <li className="muted small">empty</li>}
      </ul>
      {file && (
        <div className="file-view">
          <div className="file-view-head">
            <span>{file.path}</span>
            <a className="link" href={`/api/workspaces/${wsId}/download?path=${encodeURIComponent(file.path)}`} target="_blank" rel="noreferrer">
              Open raw
            </a>
          </div>
          {file.binary ? <p className="muted small">Binary file ({fmtSize(file.size)}).</p> : <pre>{file.content}</pre>}
        </div>
      )}
    </div>
  );
}

function fmtSize(n: number) {
  if (n >= 1 << 20) return `${(n / (1 << 20)).toFixed(1)} MB`;
  if (n >= 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${n} B`;
}
