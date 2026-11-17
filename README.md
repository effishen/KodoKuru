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
git clone https://github.com/effishen/KodoKuru.git
cd KodoKuru
./kodokuru.sh            # same as ./kodokuru.sh start
```

`kodokuru.sh` is the only thing you run, the first time and every time after. Before handing over to the `kodokuru` binary it:

1. updates the checkout (`git pull --ff-only`, skipped if you have local changes) and restarts itself if the launcher changed;
2. installs missing system packages through `sudo apt-get`: build tools, git and curl, plus QEMU, `qemu-utils`, ssh and the SFTP server for VMs;
3. adds you to the `kvm` group if needed and re-runs itself under that group, so you don't have to log out;
4. installs Rust (rustup, your user only) and Node 22 (verified download into the cache) if they're missing or too old;
5. rebuilds the web UI and the binary when the sources changed (the first build takes a few minutes; later starts take about a second).

The binary then installs Lima into the cache, runs the prerequisite check, and starts in the background.

All arguments are passed to `kodokuru` unchanged, so `./kodokuru.sh stop`, `./kodokuru.sh logs -f` and `./kodokuru.sh start --foreground --port 8443` all work. Two options are also honoured by the launcher itself: `--no-update` (no git pull, no release check) and `--no-doctor` (no package installs or prerequisite checks). Put it on your PATH with `ln -s "$PWD/kodokuru.sh" ~/.local/bin/kodokuru`.

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

KodoKuru runs its VMs with Lima inside the KodoKuru machine, so a Proxmox guest needs **nested virtualization**. You only do this once.

**1. On the Proxmox host** (its shell, as root), check whether nesting is on:

```sh
cat /sys/module/kvm_intel/parameters/nested    # Intel CPU
cat /sys/module/kvm_amd/parameters/nested      # AMD CPU
```

`Y` or `1` means it's on; skip to step 2. If you see `N` or `0`:

```sh
echo "options kvm-intel nested=Y" > /etc/modprobe.d/kvm-intel.conf   # Intel
echo "options kvm-amd nested=1"   > /etc/modprobe.d/kvm-amd.conf     # AMD
```

Then shut down all VMs and reload the module (`modprobe -r kvm_intel && modprobe kvm_intel`, or `kvm_amd`), or simply reboot the host. Check again; it should say `Y`/`1`.

**2. Set the KodoKuru VM's CPU type to `host`.** In the web UI: select the VM → **Hardware** → **Processors** → **Edit** → **Type: host**. Or on the host: `qm set <vmid> --cpu host`. Then **shut the VM down and start it again** from Proxmox; a reboot from inside the guest doesn't apply hardware changes.

**3. Check inside the VM:**

```sh
grep -cE 'vmx|svm' /proc/cpuinfo   # more than 0
ls -l /dev/kvm                     # exists
```

Run KodoKuru as your normal user, not root: Lima refuses to run as root. `kodokuru.sh` handles the `kvm` group and packages.

## Development

```sh
./kodokuru.sh start --foreground --no-update   # build and run in the foreground
cargo test --workspace
cd web && npm run dev     # UI dev server proxying to https://127.0.0.1:7443
```

Release binaries (from a `v*` tag) don't need the launcher: they update themselves from GitHub releases and install Lima on start. In a source checkout run through `kodokuru.sh`, git pull is the update mechanism.

See [docs/DESIGN.md](docs/DESIGN.md) for the architecture and the decisions behind it.

## License

GPL-3.0-or-later.
