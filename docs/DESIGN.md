# KodoKuru design

KodoKuru is a local LLM harness: a server you run on a Linux machine that gives local (or remote) models a structured way to work. They work in teams, with tools, inside isolated environments, and you can see everything they do in a web UI. This document records the architecture and the decisions behind it.

## Goals

1. **Behave like a strong, careful agent**, even with smaller local models: plan, keep a checklist, do one thing at a time, verify with evidence, report honestly. The harness enforces what it can; the model's capability is still the ceiling.
2. **Teams, not just chats.** The user talks to a lead; the lead creates role-based agents, assigns tasks and reviews reports. All of it is visible.
3. **Domain-agnostic.** The same core handles software development, reverse engineering and an LLM-run game. Templates, roles and tools change; the core doesn't.
4. **Isolated, portable workspaces.** One folder per workspace, and copying it moves it.
5. **Robust.** Survive restarts, engine outages and context limits without losing work.

## Hierarchy

```
Machine (one KodoKuru process per home folder)
 ├── Engines catalog       chat, tts, image, stt, embeddings — global
 ├── Role library          built-ins (in the binary) + user roles (home/roles)
 ├── Templates             built-ins + user templates
 └── Workspaces            isolated from each other
      ├── Environments     0..n (none / host / lima), recipe in the workspace
      ├── Roles            copies of the roles this workspace uses
      ├── Agents           lead + spawned agents; each has its own chat
      ├── Task board, world state, checklists
      └── Event log        everything, append-only (SQLite)
```

HarnessyXO's "Workspace → Project/Chat" became "Workspace/Project → Agents": the isolation unit is the workspace, and its chats are agents with roles.

## Locations

| | Contents | Lifecycle |
|---|---|---|
| **App** | `kodokuru` binary: server, web UI (embedded), built-in roles/templates | Replace any time; never writes to its own folder |
| **Home** | `kodokuru.toml`, `engines.toml`, `vault.json`, `roles/`, `templates/`, `machines/<id>.toml`, `workspaces/`, `logs/`, `run/` | Portable: copy to migrate |
| **Cache** | Lima install, VM base images, downloads, Lima socket symlinks | Disposable |

Per-machine settings (VM CPU limits, port overrides, TLS certificate) live in the home under `machines/<machine-id>`. The machine id is derived from `/etc/machine-id`, so a copied home picks the right settings on each machine and starts from defaults on a new one.

The home and workspaces carry a **schema version**. A newer KodoKuru upgrades older data (backing up the config first); an older KodoKuru refuses newer data instead of damaging it.

## Workspace folder

```
<workspace>/
  workspace.toml   id, name, template, schema version, settings (policies, limits)
  kodokuru.db      event log + projections (SQLite, WAL)
  files/           the project's files — mounted at /workspace in VMs
  roles/           roles used here (copied in, so the folder works anywhere)
  envs/<name>/     env.toml recipe (+ lima/ instance with the VM disk when keep_disk)
  attachments/
  .lock            held while open; prevents two KodoKuru processes sharing it
```

## Security

- **Vault, modelled on Proton.** A random 256-bit master key encrypts the secrets (XChaCha20-Poly1305). The master key is stored wrapped twice: once under a key derived from the **password** (Argon2id, 64 MiB, 3 passes) and once under a key derived from the **recovery key**. Only ciphertext, salts and wrapped keys are on disk. A wrong password fails authentication. Changing the password re-wraps only the master key.
- **Locked at start.** After a (re)start the engine is locked. The first web login decrypts the master key in memory, and queued agent work resumes. Logging out ends the browser session only; **Lock** forgets the key and pauses runs. Unattended auto-unlock was deliberately left out.
- **First-run setup** needs a one-time code printed in the terminal, so nobody else on the LAN can claim the instance.
- **Sessions:** HttpOnly + SameSite=Strict cookie. State-changing requests require an `X-KodoKuru` header (CSRF), and the WebSocket checks Origin. Login attempts back off after failures.
- **HTTPS by default** with a per-machine self-signed certificate; its SHA-256 fingerprint is printed at startup.
- **Agents never see secrets.** KodoKuru resolves `secret:<name>` itself when calling engines.
- File tools are sandboxed to the workspace `files/` folder: `..`, absolute paths and symlink escapes are refused.

## Engines

`engines.toml` lists every engine. **Chat** engines are an agent's brain; other kinds (TTS, image) are capabilities a role can use through tools. Each engine has `max_concurrent`, and the scheduler holds one inference slot per request. With one slot (one Mac running oMLX), agents take turns, request by request.

A background loop checks `GET /models` on every engine every 20 s, and the UI shows online/offline/busy. When an engine is unreachable, runs move to *waiting for engine* and retry with backoff (3 s → 60 s); they don't fail.

Robustness for local models:
- reasoning from `reasoning_content`/`reasoning` fields and from inline `<think>` tags (split correctly across stream chunks);
- tool calls written as `<tool_call>{…}</tool_call>` text are parsed when the server didn't;
- broken JSON arguments are repaired (code fences, trailing commas, unclosed brackets); unrecoverable ones go back to the model as a clear error;
- `tool_call_mode = "text"` describes tools in the prompt for servers without native tool calling;
- servers that ignore `stream: true` are handled.

## The agent loop

A **run** rebuilds the agent's history from the event log, calls the engine (streaming deltas to the UI), executes tool calls, and repeats until the agent gives a final reply that passes these checks:

1. **New input first.** Messages that arrived during the run are answered before finishing.
2. **Checklist nudge.** If the checklist still has open items, the harness asks the agent to finish or update them (once per run).
3. **Completion self-check.** For roles with `self_check`, after a run that used tools, the model judges its reply as COMPLETE / CONTINUE / NEEDS_INPUT (bounded: 2 per run). CONTINUE comes back as a harness message with the unmet requirement and the next action.

Limits: `max_steps_per_run` tool rounds (then *paused*), and an **autonomy budget** of agent-to-agent messages without user input (then *paused* until you speak).

The system prompt is built once per run and kept stable, so backends with prefix caching can reuse it.

**Context compaction.** When the estimated prompt exceeds about 85% of the window, older history is summarised by the model into a `Compaction` event. The cut lands on a message boundary so tool-call pairs stay intact, and the summary replaces the older events in later prompts. If the server reports overflow, KodoKuru compacts and retries once.

**Resume.** Run state is an event. On start or unlock, agents that were running, waiting, or have unanswered input are woken. A tool call with no recorded result (KodoKuru stopped mid-call) gets a synthetic result saying it was interrupted and its outcome is unknown, so the agent checks state before retrying.

**Approvals.** Tools can require approval depending on workspace policy (`command_policy`, `team_policy`, `paid_engine_policy`; host environments always ask). The run waits, and the UI shows an approval card with an optional note to the agent.

## Teams

- The **lead** (role chosen by the template) is the user's point of contact. The `team` tools let it: `list_engines`, `list_roles`, `define_role` (choosing a chat engine, tool groups and capability engines), `spawn_agent`, `assign_task` (creates a board task with acceptance criteria and wakes the agent), `message_agent`, `read_transcript`.
- **Reports route automatically.** When an agent finishes, its final reply goes as a `Report` to every agent that sent it work or questions since its previous reply, and that agent wakes up. Reports never bounce back (no ping-pong). Assigned tasks move to *review*.
- **Everyone sees the board**; each agent has its own checklist.
- **The user can step in** and message any agent directly. The UI shows each agent's chat plus an *All activity* view.

Tool groups: `fs`, `shell`, `checklist` (everyone), `board`, `team`, `env`, `state` (world state + dice, for games and simulations), `media` (TTS and image engines listed in the role).

## Environments

The `Environment` trait has three implementations:
- `none`: file tools only.
- `host`: commands on the KodoKuru machine in `files/`. Unisolated; approval required.
- `lima`: a Lima VM (QEMU/KVM on Linux) driven through `limactl`.

For Lima:
- The **recipe** (`env.toml`: image, CPUs, memory, disk, provision script) lives in the workspace, so the VM can always be rebuilt.
- With `keep_disk = true` the instance directory (disk included) lives in `envs/<name>/lima/`, so the exact VM state travels with the workspace. `LIMA_HOME` points to a short symlink in the cache because unix socket paths must stay short.
- On every start the shared-folder mount is re-pointed at the current `files/` path (`limactl edit --set`), so a workspace copied to another machine keeps working.
- Commands run as `limactl shell --workdir /workspace/... bash -lc …` with a guest-side `timeout` plus a host-side safety timeout. The whole process group is killed on timeout or stop.
- The first command auto-starts the VM; progress streams to the UI.

`kodokuru doctor [--fix]` checks KVM access, QEMU, `qemu-img`, the SFTP server (needed for shared folders), ssh, non-root, and Lima. Missing apt packages can be installed after confirmation, and Lima can be downloaded into the cache with SHA-256 verification against the official `SHA256SUMS`.

## Process model and updates

- `kodokuru.sh` (repo root) is the launcher for source checkouts. It does `git pull --ff-only` (and re-execs itself if it changed), installs system packages via sudo/apt (build tools, QEMU, ssh, SFTP server), handles the `kvm` group (re-running under `sg kvm`), installs Rust via rustup and Node 22 into the cache (SHA-256 verified) when missing, rebuilds the web UI and binary when sources changed, then execs `kodokuru` with the original arguments. It reads but never consumes options; `--no-update` and `--no-doctor` are global flags of the binary, so they pass through to any subcommand.
- `kodokuru start` runs the update check, then the prerequisite check (interactive prompts need a terminal), then re-launches itself detached (`setsid`) and waits until the port answers. `--foreground` is for development and systemd.
- Updates come from GitHub releases (`kodokuru-<arch>-linux.tar.gz` + `.sha256`). The binary is verified, swapped atomically (previous kept as `kodokuru.prev`), and KodoKuru re-execs. Updates apply only at start; a restart doesn't lose work. Source builds skip the check.
- SIGTERM/SIGINT pause runs without marking them stopped, so they resume next start.

## Web UI

React + TypeScript, built by Vite and embedded in the binary (`rust-embed`). It's a thin client: REST for actions and state, plus one WebSocket carrying events, streamed deltas, VM logs, engine health and lock changes. It catches up with `events?after=seq` after reconnecting. Light and dark themes follow the system.

## Status

Built and tested:
- **Unit tests:** vault, sandboxing, parsing, context rebuilding.
- **Integration tests against a scripted mock engine:** checklist nudge, delegation with report routing, resume after a crash, locked runtime.
- **End-to-end:** setup, login, lock and recovery over HTTPS, and UI flows in headless Chrome.
- **Lima:** create/edit commands checked against Lima 2.2.1.

Not yet verified or not built:
- Booting Lima VMs on real KVM (needs the Proxmox guest with nested virtualization).
- Network isolation for VMs (`network = false` is recorded but **not enforced**).
- Checkpoints / rewind (restore files, conversation and memory to an earlier point).
- Long-term memory across tasks, beyond the task ledger and compaction.
- File upload/attachments in the UI.
- Multi-user accounts (single user today).
- macOS host support (Lima works there; untested).

## Roadmap

1. First real runs against oMLX; tune prompts and the self-check with real models.
2. Lima VM boot on the Proxmox guest; network isolation.
3. Checkpoints and rewind using the event log plus file snapshots.
4. Attachments and file editing in the UI; diff views for edits.
5. MCP client so agents can use existing tool servers.
6. Channels and visibility rules for games (player-visible table vs. GM-private notes).
