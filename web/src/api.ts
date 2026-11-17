// Thin client for the KodoKuru API and live WebSocket.

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(`/api${path}`, {
    method,
    credentials: "same-origin",
    headers: {
      ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
      // Required on state-changing requests (CSRF protection).
      "X-KodoKuru": "1",
    },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  let data: any = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    data = { error: text };
  }
  if (!res.ok) {
    if (res.status === 401) window.dispatchEvent(new Event("kk-unauthorized"));
    throw new ApiError(res.status, data?.error ?? text ?? res.statusText);
  }
  return data as T;
}

export const api = {
  get: <T,>(p: string) => call<T>("GET", p),
  post: <T,>(p: string, b: unknown = {}) => call<T>("POST", p, b),
  put: <T,>(p: string, b: unknown) => call<T>("PUT", p, b),
  del: <T,>(p: string) => call<T>("DELETE", p),
};

type Listener = (msg: any) => void;

/** One reconnecting WebSocket shared by the whole app. */
class Live {
  private ws: WebSocket | null = null;
  private listeners = new Set<Listener>();
  private retry = 500;
  private stopped = true;
  connected = false;

  start() {
    if (!this.stopped) return;
    this.stopped = false;
    this.open();
  }

  stop() {
    this.stopped = true;
    this.ws?.close();
    this.ws = null;
  }

  private open() {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/api/ws`);
    this.ws = ws;
    ws.onopen = () => {
      this.retry = 500;
      this.connected = true;
      this.emit({ live: "connected" });
    };
    ws.onmessage = (e) => {
      try {
        this.emit(JSON.parse(e.data));
      } catch {
        /* ignore */
      }
    };
    ws.onclose = () => {
      this.connected = false;
      this.emit({ live: "disconnected" });
      if (this.stopped) return;
      setTimeout(() => this.open(), this.retry);
      this.retry = Math.min(this.retry * 2, 10000);
    };
  }

  private emit(m: any) {
    this.listeners.forEach((l) => l(m));
  }

  subscribe(l: Listener) {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  }
}

export const live = new Live();
