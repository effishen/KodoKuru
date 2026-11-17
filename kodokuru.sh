#!/usr/bin/env bash
# KodoKuru launcher: installs what's missing, keeps the checkout and build up
# to date, then runs the kodokuru binary with exactly the arguments you gave.
#
#   ./kodokuru.sh                  start (same as `./kodokuru.sh start`)
#   ./kodokuru.sh start --foreground
#   ./kodokuru.sh stop | restart | status | logs -f | doctor --fix | update
#   ./kodokuru.sh --help           all commands and options
#
# Every option belongs to the kodokuru binary and is passed through unchanged.
# This script also honours two of them itself:
#   --no-update   don't `git pull` (the binary skips its release check too)
#   --no-doctor   don't install system packages / VM prerequisites
# and reads --home / --cache to know where things live.
#
# Tip: put it on your PATH:  ln -s "$PWD/kodokuru.sh" ~/.local/bin/kodokuru

set -euo pipefail

SELF="$(readlink -f "${BASH_SOURCE[0]}")"
ROOT="$(dirname "$SELF")"
BIN="$ROOT/target/release/kodokuru"
MIN_RUST="1.89"
NODE_MAJOR="22"
NODE_MIRROR="${KODOKURU_NODE_MIRROR:-https://nodejs.org/dist}"
ORIG_ARGS=("$@")

if [ -t 2 ]; then C_Y=$'\033[1;33m' C_R=$'\033[1;31m' C_0=$'\033[0m'; else C_Y="" C_R="" C_0=""; fi
say() { printf '%skodokuru.sh:%s %s\n' "$C_Y" "$C_0" "$*" >&2; }
die() { printf '%skodokuru.sh:%s %s\n' "$C_R" "$C_0" "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# ------------------------------------------------------------ parse arguments
# Read (never consume) the options this script cares about.
CMD=""
NO_UPDATE=0
NO_DOCTOR=0
CACHE_ARG=""
INFO_ONLY=0
args=("$@")
i=0
while [ $i -lt ${#args[@]} ]; do
  a="${args[$i]}"
  case "$a" in
    --no-update) NO_UPDATE=1 ;;
    --no-doctor) NO_DOCTOR=1 ;;
    --cache) i=$((i + 1)); CACHE_ARG="${args[$i]:-}" ;;
    --cache=*) CACHE_ARG="${a#--cache=}" ;;
    --home) i=$((i + 1)) ;;
    --home=*) ;;
    -h | --help | -V | --version) INFO_ONLY=1 ;;
    -*) ;;
    *) [ -z "$CMD" ] && CMD="$a" ;;
  esac
  i=$((i + 1))
done
case "$CMD" in
  "" | start | restart | update) HEAVY=1 ;; # update + install + build
  *) HEAVY=0 ;;                              # stop/status/logs/doctor/help: just run
esac
[ $INFO_ONLY -eq 1 ] && HEAVY=0

CACHE="${CACHE_ARG:-${KODOKURU_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/kodokuru}}"
mkdir -p "$CACHE/tools"
export PATH="$HOME/.cargo/bin:$PATH"
[ -x "$CACHE/tools/node/bin/node" ] && export PATH="$CACHE/tools/node/bin:$PATH"
export KODOKURU_LAUNCHER=1

[ "$(uname -s)" = "Linux" ] || say "warning: only Linux is supported so far ($(uname -s) detected)"

if [ "$(id -u)" -eq 0 ]; then
  SUDO=""
  [ $HEAVY -eq 1 ] && say "warning: running as root — VM environments need a normal user (Lima refuses root)"
elif have sudo; then
  SUDO="sudo"
else
  SUDO="__none__"
fi

as_root() {
  if [ "$SUDO" = "__none__" ]; then
    die "need root to run: $* (install sudo or run that command as root)"
  fi
  $SUDO "$@"
}

# -------------------------------------------------------- system packages
apt_missing() {
  local missing=() p
  for p in "$@"; do
    dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q "install ok installed" || missing+=("$p")
  done
  echo "${missing[@]:-}"
}

install_packages() {
  local base=(curl git ca-certificates build-essential tar gzip)
  local vm=()
  if [ $NO_DOCTOR -eq 0 ]; then
    case "$(uname -m)" in
      aarch64 | arm64) vm+=(qemu-system-arm) ;;
      *) vm+=(qemu-system-x86) ;;
    esac
    vm+=(qemu-utils openssh-client openssh-sftp-server)
  fi
  if have apt-get && have dpkg-query; then
    local missing
    missing="$(apt_missing "${base[@]}" "${vm[@]}")"
    if [ -n "$missing" ]; then
      say "installing system packages: $missing"
      local log="$CACHE/apt-install.log"
      # shellcheck disable=SC2086
      if ! { as_root env DEBIAN_FRONTEND=noninteractive apt-get update -qq &&
        as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq $missing; } >"$log" 2>&1; then
        tail -n 20 "$log" >&2
        die "package installation failed (full log: $log)"
      fi
    fi
  else
    for c in curl git cc tar; do
      have "$c" || die "missing '$c'. Install: ${base[*]} ${vm[*]} with your package manager, then run again."
    done
    if [ ${#vm[@]} -gt 0 ] && ! have qemu-img; then
      say "for VMs install with your package manager: QEMU, qemu-img, ssh, sftp-server"
    fi
  fi
}

# KVM access: join the kvm group and re-run under it without logging out.
ensure_kvm() {
  [ $NO_DOCTOR -eq 1 ] && return 0
  if [ ! -e /dev/kvm ]; then
    say "note: /dev/kvm is missing, so VM environments won't work."
    say "      On Proxmox: enable nested virtualization on the host and set this VM's CPU type to 'host' (see README)."
    return 0
  fi
  [ "$(id -u)" -eq 0 ] && return 0
  [ -r /dev/kvm ] && [ -w /dev/kvm ] && return 0
  if ! id -nG "$USER" | tr ' ' '\n' | grep -qx kvm; then
    say "adding $USER to the kvm group for VM access"
    as_root usermod -aG kvm "$USER"
  fi
  if [ -z "${KODOKURU_KVM_REEXEC:-}" ] && have sg && grep -q '^kvm:' /etc/group; then
    say "re-running with kvm group access (no logout needed)"
    export KODOKURU_KVM_REEXEC=1
    exec sg kvm -c "$(printf '%q ' "$SELF" "${ORIG_ARGS[@]}")"
  fi
  say "note: log out and back in so the kvm group takes effect"
}

# ------------------------------------------------------------------- Rust
version_ge() { [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" = "$2" ]; }

ensure_rust() {
  if ! have cargo; then
    say "installing Rust (rustup, current user only)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
    export PATH="$HOME/.cargo/bin:$PATH"
  fi
  local v
  v="$(rustc --version | awk '{print $2}')"
  if ! version_ge "$v" "$MIN_RUST"; then
    if have rustup; then
      say "updating Rust $v → stable (need $MIN_RUST+)"
      rustup update stable --no-self-update
    else
      die "Rust $v is too old (need $MIN_RUST+) and was not installed with rustup. Install rustup: https://rustup.rs"
    fi
  fi
}

# ------------------------------------------------------------------- Node
ensure_node() {
  if have node && have npm; then
    local v
    v="$(node --version | sed 's/^v//')"
    version_ge "$v" "20.0.0" && return 0
    say "system Node $v is too old (need 20+); installing Node $NODE_MAJOR into the cache"
  else
    say "installing Node $NODE_MAJOR into the cache ($CACHE/tools/node)"
  fi
  local arch
  case "$(uname -m)" in
    x86_64) arch=x64 ;;
    aarch64 | arm64) arch=arm64 ;;
    *) die "no Node build for $(uname -m); install Node 20+ yourself" ;;
  esac
  local url="$NODE_MIRROR/latest-v${NODE_MAJOR}.x"
  local sums file sha tmp
  sums="$(curl -fsSL "$url/SHASUMS256.txt")" || die "cannot reach $url; install Node 20+ yourself (or set KODOKURU_NODE_MIRROR)"
  file="$(echo "$sums" | awk -v a="linux-$arch.tar.gz" '$2 ~ a"$" {print $2; exit}')"
  sha="$(echo "$sums" | awk -v f="$file" '$2 == f {print $1}')"
  [ -n "$file" ] || die "no Node download found for linux-$arch"
  tmp="$(mktemp -d)"
  curl -fsSL "$url/$file" -o "$tmp/$file"
  echo "$sha  $tmp/$file" | sha256sum -c --quiet - || { rm -rf "$tmp"; die "Node download checksum mismatch"; }
  rm -rf "$CACHE/tools/node.new" && mkdir -p "$CACHE/tools/node.new"
  tar -xzf "$tmp/$file" -C "$CACHE/tools/node.new" --strip-components=1
  rm -rf "$CACHE/tools/node" "$tmp" && mv "$CACHE/tools/node.new" "$CACHE/tools/node"
  export PATH="$CACHE/tools/node/bin:$PATH"
  say "installed $(node --version)"
}

# ----------------------------------------------------------------- update
update_checkout() {
  [ $NO_UPDATE -eq 1 ] && return 0
  [ -d "$ROOT/.git" ] || return 0
  cd "$ROOT"
  if ! git rev-parse --abbrev-ref --symbolic-full-name '@{u}' >/dev/null 2>&1; then
    return 0 # no upstream branch to pull from
  fi
  if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    say "local changes in the checkout — skipping git pull"
    return 0
  fi
  local before after
  before="$(git rev-parse HEAD)"
  if ! git pull --ff-only -q 2>/dev/null; then
    say "git pull failed (offline or diverged) — continuing with the current version"
    return 0
  fi
  after="$(git rev-parse HEAD)"
  if [ "$before" != "$after" ]; then
    say "updated $(git log --oneline "$before..$after" | wc -l | tr -d ' ') commit(s)"
    if ! git diff --quiet "$before" "$after" -- kodokuru.sh; then
      say "the launcher itself changed; restarting it"
      exec "$SELF" "${ORIG_ARGS[@]}"
    fi
  fi
}

# ------------------------------------------------------------------ build
newer_than() { # newer_than TARGET PATH... → true if any file in PATHs is newer
  local target="$1"
  shift
  [ -e "$target" ] || return 0
  [ -n "$(find "$@" -newer "$target" -type f -print -quit 2>/dev/null)" ]
}

build_web() {
  local w="$ROOT/web" out="$ROOT/web/dist/index.html"
  if [ -f "$out" ] && ! grep -q "has not been built" "$out" &&
    ! newer_than "$out" "$w/src" "$w/index.html" "$w/package.json" "$w/package-lock.json" "$w/vite.config.ts"; then
    return 0
  fi
  ensure_node
  cd "$w"
  if [ ! -d node_modules ] || newer_than node_modules/.package-lock.json package-lock.json; then
    say "installing web dependencies"
    npm ci --no-audit --no-fund --loglevel=error
  fi
  say "building the web UI"
  npm run build --silent >/dev/null
}

build_rust() {
  ensure_rust
  cd "$ROOT"
  if [ ! -x "$BIN" ]; then
    say "building KodoKuru (the first build takes a few minutes)"
    cargo build --release -p kodokuru
  elif newer_than "$BIN" crates assets web/dist Cargo.toml Cargo.lock; then
    say "rebuilding KodoKuru"
    cargo build --release -p kodokuru
  fi
}

# -------------------------------------------------------------------- run
if [ $HEAVY -eq 1 ]; then
  update_checkout
  install_packages
  ensure_kvm
  build_web
  build_rust
elif [ ! -x "$BIN" ]; then
  install_packages
  build_web
  build_rust
fi

exec "$BIN" "${ORIG_ARGS[@]}"
