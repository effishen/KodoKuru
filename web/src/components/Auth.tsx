import { useState } from "react";
import { api } from "../api";
import { Logo } from "./Icons";

function Shell({ title, subtitle, children }: { title: string; subtitle?: string; children: React.ReactNode }) {
  return (
    <div className="auth">
      <div className="auth-card">
        <div className="auth-brand">
          <Logo size={34} />
          <span>KodoKuru</span>
        </div>
        <h1>{title}</h1>
        {subtitle && <p className="muted">{subtitle}</p>}
        {children}
      </div>
    </div>
  );
}

export function Setup({ onDone }: { onDone: () => void }) {
  const [code, setCode] = useState("");
  const [pw, setPw] = useState("");
  const [pw2, setPw2] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [recovery, setRecovery] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  if (recovery) {
    return (
      <Shell title="Save your recovery key" subtitle="This is the only way back in if you forget your password. It is shown once.">
        <div className="recovery-key">{recovery}</div>
        <button className="btn ghost" onClick={() => navigator.clipboard?.writeText(recovery)}>
          Copy
        </button>
        <label className="check">
          <input type="checkbox" checked={saved} onChange={(e) => setSaved(e.target.checked)} /> I have stored this key somewhere safe
        </label>
        <button className="btn primary wide" disabled={!saved} onClick={onDone}>
          Continue
        </button>
      </Shell>
    );
  }

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setErr(null);
    if (pw !== pw2) return setErr("Passwords don't match.");
    setBusy(true);
    try {
      const r = await api.post<{ recovery_key: string }>("/setup", { code, password: pw });
      setRecovery(r.recovery_key);
    } catch (e: any) {
      setErr(e.message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Shell
      title="Set up KodoKuru"
      subtitle="Choose the password that logs you in and decrypts your secrets. Enter the setup code shown in the terminal where you started KodoKuru."
    >
      <form onSubmit={submit} className="form">
        <label>
          Setup code
          <input value={code} onChange={(e) => setCode(e.target.value)} placeholder="0000-0000" autoFocus />
        </label>
        <label>
          Password
          <input type="password" value={pw} onChange={(e) => setPw(e.target.value)} minLength={8} />
        </label>
        <label>
          Repeat password
          <input type="password" value={pw2} onChange={(e) => setPw2(e.target.value)} minLength={8} />
        </label>
        {err && <div className="error">{err}</div>}
        <button className="btn primary wide" disabled={busy || !code || pw.length < 8}>
          {busy ? "Creating vault…" : "Create"}
        </button>
      </form>
    </Shell>
  );
}

export function Login({ onDone, locked }: { onDone: () => void; locked: boolean }) {
  const [pw, setPw] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [recover, setRecover] = useState(false);
  const [key, setKey] = useState("");
  const [npw, setNpw] = useState("");

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setErr(null);
    try {
      if (recover) await api.post("/recover", { recovery_key: key, new_password: npw });
      else await api.post("/login", { password: pw });
      onDone();
    } catch (e: any) {
      setErr(e.message);
    } finally {
      setBusy(false);
    }
  };

  if (recover) {
    return (
      <Shell title="Recover access" subtitle="Enter your recovery key and choose a new password.">
        <form onSubmit={submit} className="form">
          <label>
            Recovery key
            <input value={key} onChange={(e) => setKey(e.target.value)} placeholder="XXXX-XXXX-…" autoFocus />
          </label>
          <label>
            New password
            <input type="password" value={npw} onChange={(e) => setNpw(e.target.value)} minLength={8} />
          </label>
          {err && <div className="error">{err}</div>}
          <button className="btn primary wide" disabled={busy || !key || npw.length < 8}>
            {busy ? "Unlocking…" : "Recover"}
          </button>
          <button type="button" className="link" onClick={() => setRecover(false)}>
            Back to login
          </button>
        </form>
      </Shell>
    );
  }

  return (
    <Shell
      title={locked ? "Unlock KodoKuru" : "Log in"}
      subtitle={locked ? "Your password decrypts the vault. Queued agent work resumes once unlocked." : undefined}
    >
      <form onSubmit={submit} className="form">
        <label>
          Password
          <input type="password" value={pw} onChange={(e) => setPw(e.target.value)} autoFocus />
        </label>
        {err && <div className="error">{err}</div>}
        <button className="btn primary wide" disabled={busy || !pw}>
          {busy ? "Unlocking…" : locked ? "Unlock" : "Log in"}
        </button>
        <button type="button" className="link" onClick={() => setRecover(true)}>
          Forgot password? Use your recovery key
        </button>
      </form>
    </Shell>
  );
}
