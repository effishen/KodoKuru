//! `kodokuru` — start, stop and inspect the KodoKuru engine.

mod daemon;
mod setup;
mod update;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use kk_home::Home;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "kodokuru", version, about = "A local LLM harness: role-based agent teams in isolated, portable workspaces")]
struct Cli {
    #[command(flatten)]
    loc: Locations,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Args, Clone)]
pub struct Locations {
    /// Home folder (settings, vault, workspaces). Default: $KODOKURU_HOME or ~/.local/share/kodokuru
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// Cache folder (VM images, downloads). Default: $KODOKURU_CACHE or ~/.cache/kodokuru
    #[arg(long, global = true)]
    cache: Option<PathBuf>,
    /// Skip update checks (GitHub release for the binary; git pull in kodokuru.sh)
    #[arg(long, global = true)]
    pub no_update: bool,
    /// Skip prerequisite checks and installation (here and in kodokuru.sh)
    #[arg(long, global = true)]
    pub no_doctor: bool,
}

impl Locations {
    pub fn home_root(&self) -> PathBuf {
        self.home.clone().unwrap_or_else(kk_home::default_home_root)
    }
    pub fn cache_root(&self) -> PathBuf {
        self.cache.clone().unwrap_or_else(kk_home::default_cache_root)
    }
    pub fn open(&self) -> Result<Home> {
        Home::open(&self.home_root(), &self.cache_root())
    }
}

#[derive(Args, Clone, Default)]
pub struct StartOpts {
    /// Stay in the foreground (development, or under systemd)
    #[arg(long)]
    foreground: bool,
    /// Port to listen on (overrides settings)
    #[arg(long)]
    port: Option<u16>,
    /// Address to bind (default from settings: 0.0.0.0)
    #[arg(long)]
    bind: Option<String>,
    /// Serve plain HTTP (only behind a TLS reverse proxy or on localhost)
    #[arg(long)]
    no_tls: bool,
    /// Internal: this process is the detached server
    #[arg(long, hide = true)]
    detached_child: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start KodoKuru (in the background unless --foreground)
    Start(StartOpts),
    /// Stop the running KodoKuru (agent runs resume on next start)
    Stop,
    /// Stop, then start
    Restart(StartOpts),
    /// Show whether KodoKuru is running and where
    Status,
    /// Show the log
    Logs {
        /// Keep following the log
        #[arg(short, long)]
        follow: bool,
        /// Lines to show
        #[arg(short = 'n', long, default_value_t = 60)]
        lines: usize,
    },
    /// Check (and with --fix, install) prerequisites for VM environments
    Doctor {
        #[arg(long)]
        fix: bool,
    },
    /// Check for and install a newer release now
    Update,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let loc = cli.loc.clone();
    match cli.cmd.unwrap_or(Cmd::Start(StartOpts::default())) {
        Cmd::Start(o) => daemon::start(&loc, o),
        Cmd::Restart(o) => {
            daemon::stop(&loc)?;
            daemon::start(&loc, o)
        }
        Cmd::Stop => daemon::stop(&loc),
        Cmd::Status => daemon::status(&loc),
        Cmd::Logs { follow, lines } => daemon::logs(&loc, follow, lines),
        Cmd::Doctor { fix } => {
            let home = loc.open()?;
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(setup::doctor(&home, if fix { setup::Mode::Fix } else { setup::Mode::Report }))
        }
        Cmd::Update => {
            let home = loc.open()?;
            let rt = tokio::runtime::Runtime::new()?;
            match rt.block_on(update::check_and_install(&home.settings.update_repo, true))? {
                update::Outcome::Updated(v) => println!("Updated to {v}. Restart KodoKuru to use it: kodokuru restart"),
                update::Outcome::UpToDate => println!("KodoKuru {} is up to date.", kk_core::VERSION),
                update::Outcome::Skipped(why) => println!("Update skipped: {why}"),
            }
            Ok(())
        }
    }
    .context("kodokuru")
}
