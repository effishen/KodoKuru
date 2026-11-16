// Mirrors the Rust types serialised by the KodoKuru API.

export type Sender = { kind: "user" } | { kind: "harness" } | { kind: "agent"; id: string };

export type MessageKind =
  | { kind: "chat" }
  | { kind: "report" }
  | { kind: "harness" }
  | { kind: "task_assignment"; task_id: string };

export type RunState =
  | { state: "idle" }
  | { state: "running" }
  | { state: "waiting_engine"; engine: string; reason: string }
  | { state: "waiting_approval" }
  | { state: "locked" }
  | { state: "stopped" }
  | { state: "paused"; reason: string }
  | { state: "error"; message: string };

export interface ToolCall {
  id: string;
  name: string;
  arguments: string;
}

export interface ChecklistItem {
  content: string;
  status: "pending" | "in_progress" | "completed";
}

export type TaskStatus = "todo" | "in_progress" | "review" | "done" | "blocked";

export interface Task {
  id: string;
  title: string;
  description: string;
  acceptance: string;
  status: TaskStatus;
  assignee: string | null;
  created_by: Sender;
  notes: { by: Sender; text: string; at: string }[];
  created_at: string;
  updated_at: string;
}

export interface AgentInfo {
  id: string;
  name: string;
  role_id: string;
  is_lead: boolean;
  created_by: Sender;
}

export interface Role {
  id: string;
  name: string;
  description: string;
  prompt: string;
  engine?: string | null;
  tools: string[];
  engines: string[];
  environment?: string | null;
  temperature?: number | null;
  reasoning_effort?: string | null;
  self_check: boolean;
}

type EventBody =
  | { type: "workspace_created"; name: string; template: string }
  | { type: "agent_created"; agent: AgentInfo }
  | { type: "role_defined"; role: Role; by: Sender }
  | { type: "message"; to: string; from: Sender; text: string; kind: MessageKind }
  | {
      type: "assistant";
      content: string;
      reasoning?: string | null;
      tool_calls: ToolCall[];
      usage?: { prompt_tokens: number; completion_tokens: number; elapsed_ms: number } | null;
      engine: string;
    }
  | { type: "tool_result"; call_id: string; name: string; output: string; is_error: boolean }
  | { type: "checklist"; items: ChecklistItem[] }
  | { type: "task_upsert"; task: Task }
  | { type: "run_state"; state: RunState }
  | { type: "approval_requested"; approval_id: string; call_id: string; tool: string; summary: string; arguments: string }
  | { type: "approval_resolved"; approval_id: string; approved: boolean; note: string }
  | { type: "compaction"; summary: string; upto_seq: number }
  | { type: "state_set"; key: string; value: unknown }
  | { type: "env_state"; name: string; state: string; detail: string }
  | { type: "notice"; level: string; text: string };

export type KEvent = EventBody & {
  seq: number;
  ts: string;
  agent_id: string | null;
  visibility: "public" | "internal";
};

export interface AgentView {
  agent: AgentInfo;
  run_state: RunState;
  running: boolean;
  checklist: ChecklistItem[];
}

export interface EnvRecipe {
  name: string;
  kind: "none" | "host" | "lima";
  description: string;
  image: string;
  cpus: number;
  memory: string;
  disk: string;
  provision: string;
  keep_disk: boolean;
  network: boolean;
}

export interface WorkspaceSettings {
  default_engine: string;
  team_policy: "auto" | "ask";
  command_policy: "auto" | "ask";
  paid_engine_policy: "auto" | "ask";
  max_agents: number;
  allowed_engines: string[];
  max_steps_per_run: number;
  autonomy_budget: number;
  default_environment?: string | null;
}

export interface WorkspaceDetail {
  manifest: { id: string; name: string; template: string; created_at: string; settings: WorkspaceSettings; workspace_prompt: string };
  root: string;
  agents: AgentView[];
  tasks: Task[];
  roles: Role[];
  envs: { recipe: EnvRecipe; status: { state: string; message?: string } }[];
  approvals: { approval_id: string; agent_id: string; tool: string; summary: string }[];
  last_seq: number;
}

export interface WorkspaceSummary {
  id: string;
  name: string;
  template: string;
  created_at: string;
  agents: number;
  running: number;
  approvals: number;
}

export interface EngineHealth {
  id: string;
  kind: string;
  status: "online" | "offline" | "unknown" | "locked";
  detail?: string;
  models: string[];
  busy: number;
  capacity: number;
}

export interface Template {
  id: string;
  name: string;
  description: string;
  lead_role: string;
  roles: string[];
  initial_agents: string[];
}

export interface AppState {
  version: string;
  setup_required: boolean;
  locked: boolean;
  authenticated: boolean;
  home: string;
  machine: string;
}
