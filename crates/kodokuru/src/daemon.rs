//! Process management: run in the foreground, or detach into the
//! background (surviving the terminal closing), plus stop/status/logs.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, IsTerminal, Seek};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::{setup, update, Locations, StartOpts};

/// println! that ignores a closed stdout (e.g. `kodokuru status | head -1`).
macro_rules! say {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($t)*);
    }};
}

#[derive(Serialize, Deserialize)]
struct RunInfo {
    pid: u32,
    version: String,
    url: String,
    started_at: chrono::DateTime<chrono::Utc>,
    tls: bool,
    #[serde(default)]
    fingerprint: String,
}

fn info_path(home: &Path) -> PathBuf {
    home.join("run").join("info.json")
}

fn read_info(home: &Path) -> Option<RunInfo> {
    serde_json::from_str(&std::fs::read_to_string(info_path(home)).ok()?).ok()
}

fn running(home: &kk_home::Home) -> Result<bool> {
    Ok(home.lock()?.is_none())
}

fn url(bind: &str, port: u16, tls: bool) -> String {
    let host = if bind == "0.0.0.0" || bind == "::" {
        kk_server::tls::primary_ip().map(|i| i.to_string()).unwrap_or_else(|| "localhost".into())
    } else {
        bind.to_string()
    };
    format!("{}://{host}:{port}", if tls { "https" } else { "http" })
}

fn print_banner(home: &kk_home::Home, info: &RunInfo) {
    say!("KodoKuru {} is running (pid {})", info.version, info.pid);
    say!("  Open:  {}", info.url);
    if info.tls && !info.fingerprint.is_empty() {
        say!("  TLS:   self-signed certificate, SHA-256 {}", info.fingerprint);
    }
    say!("  Home:  {}", home.root.display());
    if let Ok(code) = std::fs::read_to_string(home.run_dir().join("setup-code")) {
        if !kk_home::vault::exists(&home.vault_path()) {
            say!("\n  First-time setup code: {}", code.trim());
            say!("  Open the page above and enter this code to choose your password.");
        }
    } else if kk_home::vault::exists(&home.vault_path()) {
        say!("  Log in on the page to unlock; queued agent work resumes after that.");
    }
}

pub fn start(loc: &Locations, o: StartOpts) -> Result<()> {
    let home = loc.open()?;
    if running(&home)? {
        match read_info(&home.root) {
            Some(i) => print_banner(&home, &i),
            None => println!("KodoKuru is already running."),
        }
        return Ok(());
    }
    if !o.detached_child {
        let rt = tokio::runtime::Runtime::new()?;
        // 1. Update first, so the new version does the rest.
        if !o.no_update && home.settings.auto_update && std::env::var_os("KODOKURU_UPDATED").is_none() {
            match rt.block_on(update::check_and_install(&home.settings.update_repo, false)) {
                Ok(update::Outcome::Updated(v)) => {
                    println!("Updated KodoKuru to {v}; restarting…");
                    update::reexec()?;
                }
                Ok(update::Outcome::Skipped(why)) => tracing_free_note(&format!("update check skipped: {why}")),
                Ok(update::Outcome::UpToDate) => {}
                Err(e) => tracing_free_note(&format!("update check failed: {e:#}")),
            }
        }
        // 2. Prerequisites (interactive fixes need a terminal, so do them here).
        if !o.no_doctor {
            let interactive = std::io::stdin().is_terminal();
            rt.block_on(setup::doctor(&home, false, interactive))?;
        }
        drop(rt);
    }
    if o.foreground || o.detached_child {
        return run_server(home, o);
    }
    spawn_detached(&home, &o)
}

fn tracing_free_note(s: &str) {
    eprintln!("note: {s}");
}

fn spawn_detached(home: &kk_home::Home, o: &StartOpts) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    let out = std::fs::OpenOptions::new().create(true).append(true).open(home.logs_dir().join("kodokuru.out"))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--home").arg(&home.root).arg("--cache").arg(&home.cache).arg("start").arg("--detached-child");
    if let Some(p) = o.port {
        cmd.arg("--port").arg(p.to_string());
    }
    if let Some(b) = &o.bind {
        cmd.arg("--bind").arg(b);
    }
    if o.no_tls {
        cmd.arg("--no-tls");
    }
    cmd.stdin(std::process::Stdio::null()).stdout(out.try_clone()?).stderr(out);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = cmd.spawn().context("start background process")?;
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        if let Some(i) = read_info(&home.root) {
            if i.pid == pid {
                // Wait until the port accepts connections.
                if let Some(port) = i.url.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
                    if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok() {
                        print_banner(home, &i);
                        return Ok(());
                    }
                }
            }
        }
        if unsafe { libc::kill(pid as i32, 0) } != 0 {
            bail!("KodoKuru exited during startup; see `kodokuru logs`\n{}", tail_file(&home.logs_dir().join("kodokuru.out"), 15));
        }
    }
    bail!("KodoKuru did not become ready within 30s; see `kodokuru logs`")
}

fn init_logging(home: &kk_home::Home, to_stdout: bool) -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::prelude::*;
    let file = tracing_appender::rolling::never(home.logs_dir(), "kodokuru.log");
    let (nb, guard) = tracing_appender::non_blocking(file);
    let filter = tracing_subscriber::EnvFilter::try_from_env("KODOKURU_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,hyper=warn,reqwest=warn,rustls=warn"));
    let file_layer = tracing_subscriber::fmt::layer().with_writer(nb).with_ansi(false);
    let stdout_layer = to_stdout.then(|| tracing_subscriber::fmt::layer().with_target(false));
    tracing_subscriber::registry().with(filter).with(file_layer).with(stdout_layer).init();
    guard
}

fn run_server(home: kk_home::Home, o: StartOpts) -> Result<()> {
    let _lock = home.lock()?.context("another KodoKuru is already using this home folder")?;
    rotate_log(&home.logs_dir().join("kodokuru.log"));
    let _guard = init_logging(&home, o.foreground && !o.detached_child);
    let tls = home.settings.tls && !o.no_tls;
    let bind = o.bind.clone().unwrap_or_else(|| home.settings.bind.clone());
    let port = o.port.unwrap_or_else(|| home.port());
    let addr: SocketAddr = format!("{bind}:{port}")
        .parse()
        .or_else(|_| format!("[{bind}]:{port}").parse())
        .with_context(|| format!("invalid bind address {bind}"))?;

    let setup_code = if kk_home::vault::exists(&home.vault_path()) {
        let _ = std::fs::remove_file(home.run_dir().join("setup-code"));
        None
    } else {
        use rand::Rng;
        let n: u32 = rand::thread_rng().gen_range(0..100_000_000);
        let code = format!("{:04}-{:04}", n / 10_000, n % 10_000);
        kk_home::fsutil::write_atomic_mode(&home.run_dir().join("setup-code"), code.as_bytes(), Some(0o600))?;
        Some(code)
    };
    let fingerprint = if tls { kk_server::tls::ensure(&home.tls_dir())?.fingerprint } else { String::new() };
    let info = RunInfo {
        pid: std::process::id(),
        version: kk_core::VERSION.into(),
        url: url(&bind, port, tls),
        started_at: chrono::Utc::now(),
        tls,
        fingerprint,
    };
    kk_home::fsutil::write_atomic(&info_path(&home.root), serde_json::to_string_pretty(&info)?.as_bytes())?;
    if o.foreground && !o.detached_child {
        print_banner(&home, &info);
    }
    tracing::info!("KodoKuru {} starting on {} (home {})", kk_core::VERSION, info.url, home.root.display());

    let info_file = info_path(&home.root);
    let rt = tokio::runtime::Runtime::new()?;
    let res = rt.block_on(async move {
        let runtime = kk_agent::Runtime::new(home.clone())?;
        runtime.open_all().await?;
        runtime.spawn_health_loop();
        kk_server::serve(runtime, kk_server::ServeOpts { addr, tls, setup_code }).await
    });
    let _ = std::fs::remove_file(info_file);
    res
}

fn rotate_log(p: &Path) {
    if p.metadata().map(|m| m.len() > 20 << 20).unwrap_or(false) {
        let _ = std::fs::rename(p, p.with_extension("log.1"));
    }
}

pub fn stop(loc: &Locations) -> Result<()> {
    let home = loc.open()?;
    if !running(&home)? {
        println!("KodoKuru is not running.");
        return Ok(());
    }
    let info = read_info(&home.root).context("KodoKuru is running but run/info.json is missing; stop it manually")?;
    unsafe {
        libc::kill(info.pid as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        if !running(&home)? {
            let _ = std::fs::remove_file(info_path(&home.root));
            println!("KodoKuru stopped. Agent work resumes on the next start.");
            return Ok(());
        }
    }
    unsafe {
        libc::kill(info.pid as i32, libc::SIGKILL);
    }
    std::thread::sleep(Duration::from_millis(500));
    let _ = std::fs::remove_file(info_path(&home.root));
    println!("KodoKuru did not stop in 30s and was killed.");
    Ok(())
}

pub fn status(loc: &Locations) -> Result<()> {
    let home = loc.open()?;
    if !running(&home)? {
        println!("KodoKuru is not running. Home: {}", home.root.display());
        return Ok(());
    }
    match read_info(&home.root) {
        Some(i) => print_banner(&home, &i),
        None => println!("KodoKuru is running (no run info)."),
    }
    Ok(())
}

fn tail_file(p: &Path, n: usize) -> String {
    let Ok(s) = std::fs::read_to_string(p) else { return String::new() };
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

pub fn logs(loc: &Locations, follow: bool, n: usize) -> Result<()> {
    let home = loc.open()?;
    let p = home.logs_dir().join("kodokuru.log");
    println!("{}", tail_file(&p, n));
    if !follow {
        return Ok(());
    }
    let mut f = std::fs::File::open(&p)?;
    f.seek(std::io::SeekFrom::End(0))?;
    let mut r = std::io::BufReader::new(f);
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            std::thread::sleep(Duration::from_millis(300));
            continue;
        }
        print!("{line}");
    }
}
