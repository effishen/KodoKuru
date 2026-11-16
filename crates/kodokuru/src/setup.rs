//! Prerequisite check and guided installation at startup.

use anyhow::Result;
use std::io::Write;

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

/// Print the report; offer fixes when `interactive` (or apply them with `fix`).
/// Never fatal: KodoKuru works without VMs, only VM environments need these.
pub async fn doctor(home: &kk_home::Home, fix: bool, interactive: bool) -> Result<()> {
    let report = kk_env::doctor::check(&home.cache);
    if report.vm_ready() {
        if fix || !interactive {
            println!("{}", report.render());
        }
        return Ok(());
    }
    println!("VM prerequisites:\n{}\n", report.render());
    if !interactive && !fix {
        println!("VM environments will not work until these are fixed (run `kodokuru doctor --fix`).");
        return Ok(());
    }
    let apt = report.missing_apt();
    if !apt.is_empty() && which("apt-get") {
        let cmd = format!("sudo apt-get install -y {}", apt.join(" "));
        if fix || ask(&format!("Install missing packages with `{cmd}`?")) {
            let st = std::process::Command::new("sudo").args(["apt-get", "install", "-y"]).args(&apt).status()?;
            if !st.success() {
                println!("Package installation failed ({st}).");
            }
        }
    }
    if !report.checks.iter().find(|c| c.name == "lima").is_some_and(|c| c.ok)
        && (fix || ask(&format!("Download and install Lima {} into the KodoKuru cache?", kk_env::lima::LIMA_VERSION)))
    {
        let progress = |l: String| println!("  {l}");
        if let Err(e) = kk_env::lima::install_managed(&home.cache, &progress).await {
            println!("Lima installation failed: {e:#}");
        }
    }
    let kvm = report.checks.iter().find(|c| c.name == "kvm");
    if let Some(k) = kvm.filter(|k| !k.ok && std::path::Path::new("/dev/kvm").exists()) {
        let user = std::env::var("USER").unwrap_or_default();
        if !user.is_empty() && (fix || ask(&format!("Add {user} to the kvm group ({})?", k.detail))) {
            let st = std::process::Command::new("sudo").args(["usermod", "-aG", "kvm", &user]).status()?;
            if st.success() {
                println!("Added. Log out and back in (or run `newgrp kvm`), then start KodoKuru again.");
            }
        }
    }
    let after = kk_env::doctor::check(&home.cache);
    if !after.vm_ready() {
        println!("\nStill missing:\n{}", after.checks.iter().filter(|c| !c.ok).map(|c| {
            format!("  {} — {}", c.name, c.hint.clone().unwrap_or_default())
        }).collect::<Vec<_>>().join("\n"));
        println!("KodoKuru will run; VM environments need these fixed.\n");
    } else {
        println!("All VM prerequisites are in place.\n");
    }
    Ok(())
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}
