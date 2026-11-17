//! Prerequisite check and installation at startup.
//!
//! What can be installed without root (Lima, into the cache, checksum
//! verified) is installed automatically. System packages and the `kvm`
//! group need sudo: done automatically with `Fix`, after a prompt in
//! `Interactive`, and in `Auto` only when sudo needs no password.
//! Never fatal: KodoKuru works without VMs.

use anyhow::Result;
use std::io::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Print the report only (`kodokuru doctor`).
    Report,
    /// Install everything, no questions (`kodokuru doctor --fix`).
    Fix,
    /// Startup with a terminal: automatic where no sudo is needed, ask otherwise.
    Interactive,
    /// Startup without a terminal: automatic where possible, hints otherwise.
    Auto,
}

fn ask(q: &str) -> bool {
    print!("{q} [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    // EOF (no terminal attached) or an error means "no".
    if !matches!(std::io::stdin().read_line(&mut s), Ok(n) if n > 0) {
        println!();
        return false;
    }
    let s = s.trim().to_lowercase();
    s.is_empty() || s == "y" || s == "yes"
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Run a command as root: directly when we are root, else through sudo
/// (non-interactive sudo when `quiet`).
fn as_root(args: &[&str], quiet: bool) -> std::io::Result<std::process::ExitStatus> {
    if is_root() {
        std::process::Command::new(args[0]).args(&args[1..]).status()
    } else {
        let mut c = std::process::Command::new("sudo");
        if quiet {
            c.arg("-n");
        }
        c.args(args).status()
    }
}

fn passwordless_sudo() -> bool {
    is_root()
        || std::process::Command::new("sudo")
            .args(["-n", "true"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
}

/// Decide whether to do a sudo step in this mode.
fn may_sudo(mode: Mode, question: &str) -> bool {
    match mode {
        Mode::Report => false,
        Mode::Fix => true,
        Mode::Interactive => passwordless_sudo() || ask(question),
        Mode::Auto => passwordless_sudo(),
    }
}

pub async fn doctor(home: &kk_home::Home, mode: Mode) -> Result<()> {
    let report = kk_env::doctor::check(&home.cache);
    if report.vm_ready() {
        if matches!(mode, Mode::Report | Mode::Fix) {
            println!("{}", report.render());
        }
        return Ok(());
    }
    if mode == Mode::Report {
        println!("VM prerequisites:\n{}\n\nRun `kodokuru doctor --fix` to install what's missing.", report.render());
        return Ok(());
    }

    // Lima: no root needed, verified download into the cache — just do it.
    if !report.checks.iter().find(|c| c.name == "lima").is_some_and(|c| c.ok) {
        println!("Installing Lima {} into {} …", kk_env::lima::LIMA_VERSION, home.cache.display());
        let progress = |l: String| println!("  {l}");
        if let Err(e) = kk_env::lima::install_managed(&home.cache, &progress).await {
            println!("Lima installation failed: {e:#}");
        }
    }

    let apt = report.missing_apt();
    if !apt.is_empty() {
        if which("apt-get") {
            let cmd = format!("sudo apt-get install -y {}", apt.join(" "));
            if may_sudo(mode, &format!("Install missing packages with `{cmd}`?")) {
                println!("Installing: {}", apt.join(" "));
                let quiet = mode == Mode::Auto;
                let mut args = vec!["env", "DEBIAN_FRONTEND=noninteractive", "apt-get", "install", "-y"];
                args.extend(apt.iter().map(String::as_str));
                match as_root(&args, quiet) {
                    Ok(s) if s.success() => {}
                    Ok(s) => println!("Package installation failed ({s})."),
                    Err(e) => println!("Package installation failed: {e}"),
                }
            }
        } else {
            println!("Install these packages with your package manager: {}", apt.join(" "));
        }
    }

    if let Some(k) = report.checks.iter().find(|c| c.name == "kvm" && !c.ok) {
        let user = std::env::var("USER").unwrap_or_default();
        if std::path::Path::new("/dev/kvm").exists()
            && !user.is_empty()
            && !is_root()
            && may_sudo(mode, &format!("Add {user} to the kvm group ({})?", k.detail))
            && matches!(as_root(&["usermod", "-aG", "kvm", &user], mode == Mode::Auto), Ok(s) if s.success())
        {
            println!("Added {user} to the kvm group. It takes effect in new logins (kodokuru.sh handles this for you).");
        }
    }

    let after = kk_env::doctor::check(&home.cache);
    if !after.vm_ready() {
        println!(
            "\nVM environments are not ready yet (KodoKuru itself runs fine):\n{}\n",
            after
                .checks
                .iter()
                .filter(|c| !c.ok)
                .map(|c| format!("  {} — {}", c.name, c.hint.clone().unwrap_or_default()))
                .collect::<Vec<_>>()
                .join("\n")
        );
    } else {
        println!("All VM prerequisites are in place.");
    }
    Ok(())
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}
