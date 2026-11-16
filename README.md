# KodoKuru

A local LLM harness written in Rust. You talk to a **lead agent**; it plans the work, builds a team of role-based agents when the job needs one, hands out tasks, and reports back. Every agent's conversation, tool calls and checklist are visible in a web UI modelled on the Claude interface.

Each **workspace** is an isolated, portable folder with its own agents, files, VM environments and history. Use it for software projects, reverse engineering, an LLM-run game, or anything else: templates set up the roles and environments, and the lead can define new roles and pick engines itself.

- **Works like a careful engineer.** Agents plan with a checklist, work one step at a time, verify each step with tools, and get a completion self-check before a reply counts as done.
- **Teams.** The lead can spawn agents, assign tasks with acceptance criteria, and get reports back.
- **Any OpenAI-compatible engine** (oMLX, llama.cpp, vLLM, Ollama…), plus text-to-speech and image engines as tools.
- **Survives restarts and outages.** Agent work resumes after a restart, waits while an engine is offline, and compacts long conversations to fit the context window.
- **Isolated VMs per workspace** through Lima, built from a recipe stored in the workspace.
- **One password** logs you in and decrypts your secrets, Proton-style, with a recovery key.
- **Portable.** Copy the home folder (or one workspace folder) to another machine and open it there.

## Quick start (Linux)

```sh
# Build (Rust 1.89+ and Node 20+)
cd web && npm ci && npm run build && cd ..
cargo build --release
cp target/release/kodokuru ~/.local/bin/

kodokuru start          # checks prerequisites, then runs in the background
```

`start` prints the address (HTTPS with a self-signed certificate, port 7443) and, the first time, a **setup code**. Open the address, enter the code and choose your password. Save the recovery key it shows.

Then in **Settings**:

1. **Secrets:** add your engine's API key, for example `omlx`.
2. **Engines:** describe your model server, for example oMLX on a Mac:

   ```toml
   default_chat = "omlx"

   [[engine]]
   id = "omlx"
   kind = "chat"
   base_url = "http://192.168.1.50:8000/v1"
   model = "Qwen3-Coder-30B-A3B-Instruct-4bit"
   api_key = "secret:omlx"
   context_window = 65536
   max_concurrent = 1
   notes = "Local coding model on the Mac. Good tool use."
   ```

Create a workspace from a template and start talking to its lead.

## Commands

| Command | |
|---|---|
| `kodokuru start [--foreground] [--port N] [--no-tls]` | Start (in the background unless `--foreground`) |
| `kodokuru stop` / `restart` / `status` | Agent work resumes on the next start |
| `kodokuru logs [-f]` | Show or follow the log |
| `kodokuru doctor [--fix]` | Check and install VM prerequisites (KVM, QEMU, Lima…) |
| `kodokuru update` | Install the newest release now (also checked on every start) |

Global options: `--home DIR` (default `$KODOKURU_HOME` or `~/.local/share/kodokuru`) and `--cache DIR` (default `~/.cache/kodokuru`).

## Where things live

- **App:** the `kodokuru` binary, including the web UI and built-in roles and templates. Replace it any time.
- **Home:** settings, engines, encrypted vault, role library and workspaces. Copy it to migrate.
- **Cache:** VM base images, downloads and the managed Lima. Safe to delete.

## VMs on Proxmox

KodoKuru runs its VMs with Lima inside the KodoKuru machine. On a Proxmox guest, enable nested virtualization on the host and set the guest's CPU type to `host`. Then run `kodokuru doctor --fix` as your normal user (not root).

## Development

```sh
cargo test --workspace
cargo run -p kodokuru -- start --foreground --no-update
cd web && npm run dev     # UI dev server proxying to https://127.0.0.1:7443
```

See [docs/DESIGN.md](docs/DESIGN.md) for the architecture and the decisions behind it.

## License

GPL-3.0-or-later.
