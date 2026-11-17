//! Self-update from GitHub releases. The app folder holds only the binary,
//! so replacing it never touches user data. The previous binary is kept as
//! `kodokuru.prev` for rollback.
//!
//! Release assets: `kodokuru-<arch>-<os>.tar.gz` containing `kodokuru`, and
//! `kodokuru-<arch>-<os>.tar.gz.sha256`.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::time::Duration;

pub enum Outcome {
    Updated(String),
    UpToDate,
    Skipped(String),
}

fn parse_version(v: &str) -> Option<Vec<u64>> {
    let v = v.trim().trim_start_matches('v');
    let core = v.split(['-', '+']).next()?;
    core.split('.').map(|p| p.parse().ok()).collect()
}

pub fn newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

pub fn asset_name() -> String {
    format!("kodokuru-{}-{}.tar.gz", std::env::consts::ARCH, std::env::consts::OS)
}

pub async fn check_and_install(repo: &str, force: bool) -> Result<Outcome> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let exe_s = exe.to_string_lossy();
    let source_build = exe_s.contains("/target/debug/") || exe_s.contains("/target/release/");
    // In a source checkout run through kodokuru.sh, git pull is the update.
    if source_build && (!force || std::env::var_os("KODOKURU_LAUNCHER").is_some()) {
        let why = if std::env::var_os("KODOKURU_LAUNCHER").is_some() {
            "source checkout — kodokuru.sh keeps it up to date with git pull and rebuilds"
        } else {
            "running from a source build (use ./kodokuru.sh, or git pull && cargo build)"
        };
        return Ok(Outcome::Skipped(why.into()));
    }
    let http = reqwest::Client::builder()
        .user_agent(format!("kodokuru/{}", kk_core::VERSION))
        .timeout(Duration::from_secs(20))
        .build()?;
    let resp = http.get(format!("https://api.github.com/repos/{repo}/releases/latest")).send().await?;
    if resp.status().as_u16() == 404 {
        return Ok(Outcome::Skipped("no published releases yet".into()));
    }
    let rel: serde_json::Value = resp.error_for_status()?.json().await?;
    let tag = rel["tag_name"].as_str().unwrap_or_default().to_string();
    if !newer(&tag, kk_core::VERSION) {
        return Ok(Outcome::UpToDate);
    }
    let name = asset_name();
    let find = |n: &str| {
        rel["assets"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["name"] == n))
            .and_then(|x| x["browser_download_url"].as_str().map(str::to_string))
    };
    let Some(url) = find(&name) else {
        return Ok(Outcome::Skipped(format!("release {tag} has no {name}")));
    };
    let sum_url = find(&format!("{name}.sha256")).with_context(|| format!("release {tag} has no checksum for {name}"))?;
    let dl = reqwest::Client::builder().user_agent("kodokuru").timeout(Duration::from_secs(600)).build()?;
    let expected = dl.get(sum_url).send().await?.error_for_status()?.text().await?;
    let expected = expected.split_whitespace().next().unwrap_or_default().to_lowercase();
    let bytes = dl.get(url).send().await?.error_for_status()?.bytes().await?;
    let got = hex::encode(Sha256::digest(&bytes));
    if got != expected {
        bail!("checksum mismatch for {name}");
    }
    let dir = exe.parent().context("binary has no folder")?;
    let staged = dir.join(".kodokuru.new");
    {
        let gz = flate2::read::GzDecoder::new(&bytes[..]);
        let mut ar = tar::Archive::new(gz);
        let mut found = false;
        for entry in ar.entries()? {
            let mut e = entry?;
            if e.path()?.file_name().is_some_and(|n| n == "kodokuru") {
                e.unpack(&staged).with_context(|| format!("cannot write into {} (is it writable?)", dir.display()))?;
                found = true;
                break;
            }
        }
        if !found {
            bail!("archive does not contain a kodokuru binary");
        }
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    let prev = dir.join("kodokuru.prev");
    let _ = std::fs::remove_file(&prev);
    std::fs::hard_link(&exe, &prev).or_else(|_| std::fs::copy(&exe, &prev).map(|_| ()))?;
    std::fs::rename(&staged, &exe)?;
    Ok(Outcome::Updated(tag))
}

/// Replace this process with the (new) binary, same arguments.
pub fn reexec() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let err = std::process::Command::new(exe).args(args).env("KODOKURU_UPDATED", "1").exec();
    Err(err.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("v0.2.0", "0.1.9"));
        assert!(newer("0.10.0", "0.9.0"));
        assert!(!newer("v0.1.0", "0.1.0"));
        assert!(!newer("garbage", "0.1.0"));
        assert!(newer("1.0.0-rc1", "0.9.9"));
    }
}
