import { useCallback, useEffect, useRef, useState } from "react";
import { api, live } from "./api";
import type { EngineHealth, KEvent, WorkspaceDetail } from "./types";

export interface Stream {
  text: string;
  reasoning: string;
}

const DETAIL_EVENTS = new Set([
  "agent_created",
  "task_upsert",
  "run_state",
  "approval_requested",
  "approval_resolved",
  "env_state",
  "checklist",
  "role_defined",
]);

/** Everything about one open workspace, kept live over the WebSocket. */
export function useWorkspace(id: string | null) {
  const [detail, setDetail] = useState<WorkspaceDetail | null>(null);
  const [events, setEvents] = useState<KEvent[]>([]);
  const [streams, setStreams] = useState<Record<string, Stream>>({});
  const [envLogs, setEnvLogs] = useState<Record<string, string[]>>({});
  const [error, setError] = useState<string | null>(null);
  const lastSeq = useRef(0);
  const timer = useRef<number | null>(null);

  const refresh = useCallback(() => {
    if (!id) return;
    if (timer.current) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => {
      api
        .get<WorkspaceDetail>(`/workspaces/${id}`)
        .then(setDetail)
        .catch((e) => setError(String(e.message ?? e)));
    }, 120);
  }, [id]);

  const catchUp = useCallback(async () => {
    if (!id) return;
    const r = await api.get<{ events: KEvent[] }>(`/workspaces/${id}/events?after=${lastSeq.current}&limit=20000`);
    if (r.events.length) {
      lastSeq.current = r.events[r.events.length - 1].seq;
      setEvents((prev) => {
        const known = new Set(prev.map((e) => e.seq));
        return [...prev, ...r.events.filter((e) => !known.has(e.seq))];
      });
    }
  }, [id]);

  useEffect(() => {
    setDetail(null);
    setEvents([]);
    setStreams({});
    setEnvLogs({});
    setError(null);
    lastSeq.current = 0;
    if (!id) return;
    refresh();
    catchUp().catch((e) => setError(String(e.message ?? e)));
    return live.subscribe((m) => {
      if (m.live === "connected" || m.live === "resync") {
        catchUp().catch(() => {});
        refresh();
        return;
      }
      if (m.workspace_id !== id) return;
      if (m.live === "event") {
        const ev: KEvent = m.event;
        if (ev.seq <= lastSeq.current) return;
        lastSeq.current = ev.seq;
        setEvents((prev) => [...prev, ev]);
        if ((ev.type === "assistant" || ev.type === "run_state") && ev.agent_id) {
          setStreams((s) => {
            if (!s[ev.agent_id!]) return s;
            const n = { ...s };
            delete n[ev.agent_id!];
            return n;
          });
        }
        if (DETAIL_EVENTS.has(ev.type)) refresh();
      } else if (m.live === "delta") {
        setStreams((s) => {
          const cur = s[m.agent_id] ?? { text: "", reasoning: "" };
          return {
            ...s,
            [m.agent_id]: { text: cur.text + (m.text ?? ""), reasoning: cur.reasoning + (m.reasoning ?? "") },
          };
        });
      } else if (m.live === "env_log") {
        setEnvLogs((l) => ({ ...l, [m.name]: [...(l[m.name] ?? []), m.line].slice(-300) }));
      }
    });
  }, [id, refresh, catchUp]);

  return { detail, events, streams, envLogs, error, refresh };
}

/** Engine health and lock state from the live socket. */
export function useGlobalLive(onLocked: () => void, onWorkspaces: () => void) {
  const [health, setHealth] = useState<EngineHealth[]>([]);
  const [connected, setConnected] = useState(live.connected);
  useEffect(() => {
    return live.subscribe((m) => {
      if (m.live === "hello") setHealth(m.health ?? []);
      else if (m.live === "engine_health") setHealth(m.health);
      else if (m.live === "lock" && m.locked) onLocked();
      else if (m.live === "workspaces_changed") onWorkspaces();
      else if (m.live === "connected") setConnected(true);
      else if (m.live === "disconnected") setConnected(false);
    });
  }, [onLocked, onWorkspaces]);
  return { health, setHealth, connected };
}

export function useLocalStorage<T>(key: string, initial: T): [T, (v: T) => void] {
  const [v, setV] = useState<T>(() => {
    try {
      const s = localStorage.getItem(key);
      return s ? (JSON.parse(s) as T) : initial;
    } catch {
      return initial;
    }
  });
  const set = useCallback(
    (nv: T) => {
      setV(nv);
      try {
        localStorage.setItem(key, JSON.stringify(nv));
      } catch {
        /* storage unavailable */
      }
    },
    [key],
  );
  return [v, set];
}
