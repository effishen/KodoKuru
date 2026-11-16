import { useEffect, useState } from "react";
import { api } from "../api";
import type { AppState, EngineHealth, Role, WorkspaceSettings } from "../types";

type Tab = "engines" | "secrets" | "roles" | "workspace" | "security" | "about";

export function Settings({ app, health, wsId, onClose }: { app: AppState; health: EngineHealth[]; wsId: string | null; onClose: () => void }) {
  const [tab, setTab] = useState<Tab>("engines");
  return (
    <div className="settings">
      <div className="settings-head">
        <h1>Settings</h1>
        <button className="btn ghost" onClick={onClose}>
          Done
        </button>
      </div>
      <div className="tabs">
        {(["engines", "secrets", "roles", "workspace", "security", "about"] as Tab[]).map((t) => (
          <button key={t} className={tab === t ? "on" : ""} onClick={() => setTab(t)} disabled={t === "workspace" && !wsId}>
            {t[0].toUpperCase() + t.slice(1)}
          </button>
        ))}
      </div>
      <div className="settings-body">
        {tab === "engines" && <Engines health={health} />}
        {tab === "secrets" && <Secrets />}
        {tab === "roles" && <Roles />}
        {tab === "workspace" && wsId && <WorkspaceSettingsView wsId={wsId} />}
        {tab === "security" && <Security />}
        {tab === "about" && <About app={app} />}
      </div>
    </div>
  );
}

function Engines({ health }: { health: EngineHealth[] }) {
  const [raw, setRaw] = useState("");
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [h, setH] = useState(health);
  useEffect(() => setH(health), [health]);
  useEffect(() => {
    api.get<{ raw: string; health: EngineHealth[] }>("/engines").then((r) => (setRaw(r.raw), setH(r.health)));
  }, []);
  const save = async () => {
    try {
      await api.put("/engines", { raw });
      setMsg({ ok: true, text: "Saved." });
      const r = await api.post<{ health: EngineHealth[] }>("/engines/check");
      setH(r.health);
    } catch (e: any) {
      setMsg({ ok: false, text: e.message });
    }
  };
  return (
    <div>
      <p className="muted">
        The engine catalog every workspace picks from. <code>kind</code> is <code>chat</code>, <code>tts</code>, <code>image</code>, <code>stt</code> or <code>embeddings</code>. Keep API keys in Secrets and reference them as <code>api_key = "secret:name"</code>. Agents read <code>notes</code> and <code>cost</code> when choosing engines.
      </p>
      <table className="health">
        <tbody>
          {h.map((e) => (
            <tr key={e.id}>
              <td>
                <span className={`dot-s ${e.status}`} /> <b>{e.id}</b>
              </td>
              <td className="muted">{e.kind}</td>
              <td>{e.status}</td>
              <td className="muted small">{e.capacity ? `${e.busy}/${e.capacity} busy` : ""}</td>
              <td className="muted small">{e.detail}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <textarea className="code" value={raw} onChange={(e) => setRaw(e.target.value)} spellCheck={false} rows={22} />
      <div className="row-actions">
        <button className="btn primary" onClick={save}>
          Save engines.toml
        </button>
        {msg && <span className={msg.ok ? "ok" : "error"}>{msg.text}</span>}
      </div>
    </div>
  );
}

function Secrets() {
  const [names, setNames] = useState<string[]>([]);
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const load = () => api.get<{ names: string[] }>("/secrets").then((r) => setNames(r.names));
  useEffect(() => {
    load();
  }, []);
  const add = async () => {
    try {
      await api.put(`/secrets/${encodeURIComponent(name)}`, { value });
      setName("");
      setValue("");
      setErr(null);
      load();
    } catch (e: any) {
      setErr(e.message);
    }
  };
  return (
    <div>
      <p className="muted">Secrets are encrypted in the home folder's vault with a key only your password (or recovery key) can unlock. Values are never shown again or sent to agents.</p>
      <ul className="secret-list">
        {names.map((n) => (
          <li key={n}>
            <code>secret:{n}</code>
            <button className="link danger" onClick={() => confirm(`Delete secret ${n}?`) && api.del(`/secrets/${n}`).then(load)}>
              delete
            </button>
          </li>
        ))}
        {!names.length && <li className="muted">No secrets yet.</li>}
      </ul>
      <div className="inline-form">
        <input placeholder="name, e.g. omlx" value={name} onChange={(e) => setName(e.target.value)} />
        <input placeholder="value" type="password" value={value} onChange={(e) => setValue(e.target.value)} />
        <button className="btn primary" disabled={!name || !value} onClick={add}>
          Save
        </button>
      </div>
      {err && <div className="error">{err}</div>}
    </div>
  );
}

function Roles() {
  const [roles, setRoles] = useState<{ role: Role; source: string }[]>([]);
  const [groups, setGroups] = useState<[string, string][]>([]);
  const [edit, setEdit] = useState<Role | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const load = () => api.get<{ roles: { role: Role; source: string }[]; groups: [string, string][] }>("/roles").then((r) => (setRoles(r.roles), setGroups(r.groups)));
  useEffect(() => {
    load();
  }, []);
  const save = async () => {
    if (!edit) return;
    try {
      await api.put(`/roles/${encodeURIComponent(edit.id)}`, edit);
      setEdit(null);
      setErr(null);
      load();
    } catch (e: any) {
      setErr(e.message);
    }
  };
  if (edit) {
    const set = (k: keyof Role, v: any) => setEdit({ ...edit, [k]: v });
    return (
      <div className="form role-form">
        <label>
          Id <input value={edit.id} onChange={(e) => set("id", e.target.value)} />
        </label>
        <label>
          Name <input value={edit.name} onChange={(e) => set("name", e.target.value)} />
        </label>
        <label>
          Description <input value={edit.description} onChange={(e) => set("description", e.target.value)} />
        </label>
        <label>
          Chat engine (empty = default) <input value={edit.engine ?? ""} onChange={(e) => set("engine", e.target.value || null)} />
        </label>
        <label>
          Tool groups / tools (comma separated)
          <input value={edit.tools.join(", ")} onChange={(e) => set("tools", e.target.value.split(",").map((s) => s.trim()).filter(Boolean))} />
        </label>
        <div className="muted small">{groups.map(([g, d]) => `${g}: ${d}`).join(" · ")}</div>
        <label>
          Capability engines (tts/image…, comma separated)
          <input value={edit.engines.join(", ")} onChange={(e) => set("engines", e.target.value.split(",").map((s) => s.trim()).filter(Boolean))} />
        </label>
        <label>
          Default environment <input value={edit.environment ?? ""} onChange={(e) => set("environment", e.target.value || null)} />
        </label>
        <label className="check">
          <input type="checkbox" checked={edit.self_check} onChange={(e) => set("self_check", e.target.checked)} /> Completion self-check before final replies
        </label>
        <label>
          Instructions
          <textarea rows={12} value={edit.prompt} onChange={(e) => set("prompt", e.target.value)} />
        </label>
        {err && <div className="error">{err}</div>}
        <div className="row-actions">
          <button className="btn primary" onClick={save}>
            Save to library
          </button>
          <button className="btn ghost" onClick={() => setEdit(null)}>
            Cancel
          </button>
        </div>
      </div>
    );
  }
  return (
    <div>
      <p className="muted">
        The role library. Built-in roles ship with KodoKuru; saving one creates your own copy that overrides it. Workspaces keep their own copies of the roles they use, and the lead can define new roles itself.
      </p>
      <button
        className="btn"
        onClick={() => setEdit({ id: "", name: "", description: "", prompt: "", engine: null, tools: ["fs", "checklist", "board"], engines: [], environment: null, self_check: true })}
      >
        New role
      </button>
      <div className="role-list">
        {roles.map(({ role, source }) => (
          <div key={role.id} className="role-card" onClick={() => setEdit({ ...role })}>
            <div>
              <b>{role.name}</b> <span className="muted small">{role.id}</span> <span className={`pill src-${source}`}>{source}</span>
            </div>
            <div className="muted small">{role.description}</div>
            <div className="small">
              engine: {role.engine ?? "default"} · tools: {role.tools.join(", ")}
              {role.engines.length ? ` · engines: ${role.engines.join(", ")}` : ""}
            </div>
            {source !== "builtin" && (
              <button
                className="link danger small"
                onClick={(e) => {
                  e.stopPropagation();
                  if (confirm(source === "override" ? "Remove your override and restore the built-in role?" : "Delete this role?")) api.del(`/roles/${role.id}`).then(load);
                }}
              >
                {source === "override" ? "restore built-in" : "delete"}
              </button>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

function WorkspaceSettingsView({ wsId }: { wsId: string }) {
  const [s, setS] = useState<WorkspaceSettings | null>(null);
  const [root, setRoot] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  useEffect(() => {
    api.get<any>(`/workspaces/${wsId}`).then((r) => (setS(r.manifest.settings), setRoot(r.root)));
  }, [wsId]);
  if (!s) return null;
  const set = (k: keyof WorkspaceSettings, v: any) => setS({ ...s, [k]: v });
  const policy = (k: "team_policy" | "command_policy" | "paid_engine_policy", label: string) => (
    <label>
      {label}
      <select value={s[k]} onChange={(e) => set(k, e.target.value)}>
        <option value="auto">Automatic</option>
        <option value="ask">Ask me</option>
      </select>
    </label>
  );
  return (
    <div className="form">
      {policy("team_policy", "Lead may define roles and spawn agents")}
      {policy("command_policy", "Shell commands")}
      {policy("paid_engine_policy", "Paid engines")}
      <label>
        Default chat engine (empty = global default) <input value={s.default_engine} onChange={(e) => set("default_engine", e.target.value)} />
      </label>
      <label>
        Allowed engines (comma separated, empty = all)
        <input value={s.allowed_engines.join(", ")} onChange={(e) => set("allowed_engines", e.target.value.split(",").map((x) => x.trim()).filter(Boolean))} />
      </label>
      <label>
        Default environment <input value={s.default_environment ?? ""} onChange={(e) => set("default_environment", e.target.value || null)} />
      </label>
      <label>
        Max agents <input type="number" value={s.max_agents} onChange={(e) => set("max_agents", +e.target.value)} />
      </label>
      <label>
        Max tool steps per run <input type="number" value={s.max_steps_per_run} onChange={(e) => set("max_steps_per_run", +e.target.value)} />
      </label>
      <label>
        Autonomy budget (agent-to-agent messages without your input)
        <input type="number" value={s.autonomy_budget} onChange={(e) => set("autonomy_budget", +e.target.value)} />
      </label>
      <div className="row-actions">
        <button className="btn primary" onClick={() => api.put(`/workspaces/${wsId}/settings`, s).then(() => setMsg("Saved."), (e) => setMsg(e.message))}>
          Save
        </button>
        <button
          className="btn ghost"
          onClick={() => api.post<{ path: string }>(`/workspaces/${wsId}/prepare-migration`).then((r) => setMsg(`VMs stopped and database flushed. Stop KodoKuru, then copy ${r.path}.`), (e) => setMsg(e.message))}
        >
          Prepare for migration
        </button>
        {msg && <span className="muted">{msg}</span>}
      </div>
      <p className="muted small">Folder: {root}</p>
    </div>
  );
}

function Security() {
  const [cur, setCur] = useState("");
  const [nw, setNw] = useState("");
  const [pw, setPw] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  const [key, setKey] = useState<string | null>(null);
  return (
    <div className="form">
      <h3>Change password</h3>
      <label>
        Current password <input type="password" value={cur} onChange={(e) => setCur(e.target.value)} />
      </label>
      <label>
        New password <input type="password" value={nw} onChange={(e) => setNw(e.target.value)} />
      </label>
      <button className="btn primary" disabled={!cur || nw.length < 8} onClick={() => api.post("/password", { current: cur, new: nw }).then(() => (setMsg("Password changed."), setCur(""), setNw("")), (e) => setMsg(e.message))}>
        Change password
      </button>
      <h3>Recovery key</h3>
      <p className="muted small">Creates a new recovery key; the old one stops working.</p>
      <label>
        Password <input type="password" value={pw} onChange={(e) => setPw(e.target.value)} />
      </label>
      <button className="btn" disabled={!pw} onClick={() => api.post<{ recovery_key: string }>("/recovery-key", { password: pw }).then((r) => (setKey(r.recovery_key), setPw("")), (e) => setMsg(e.message))}>
        New recovery key
      </button>
      {key && <div className="recovery-key">{key}</div>}
      <h3>Lock</h3>
      <p className="muted small">Forget the key in memory. Agent runs pause and resume after the next login.</p>
      <button className="btn ghost" onClick={() => api.post("/lock").then(() => location.reload())}>
        Lock now
      </button>
      {msg && <div className="muted">{msg}</div>}
    </div>
  );
}

function About({ app }: { app: AppState }) {
  const [doc, setDoc] = useState<any>(null);
  useEffect(() => {
    api.get("/doctor").then(setDoc);
  }, []);
  return (
    <div>
      <p>
        KodoKuru <b>{app.version}</b> on <b>{app.machine}</b>
      </p>
      <p className="muted small">Home: {app.home}</p>
      <h3>VM prerequisites</h3>
      {doc && (
        <table className="health">
          <tbody>
            {doc.report.checks.map((c: any) => (
              <tr key={c.name}>
                <td>
                  <span className={`dot-s ${c.ok ? "online" : "offline"}`} /> {c.name}
                </td>
                <td className="small">{c.detail}</td>
                <td className="muted small">{c.ok ? "" : c.hint}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <p className="muted small">Fix missing items with <code>kodokuru doctor --fix</code> in a terminal on the KodoKuru machine.</p>
    </div>
  );
}
