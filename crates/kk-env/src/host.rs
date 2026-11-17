//! `host` environment: commands run on the KodoKuru machine inside the
//! workspace files folder. There is no isolation — use for trusted work or
//! testing only. Also provides the process runner the Lima backend reuses.

use anyhow::{Context, Result};
use async_trait::async_trait;
use kk_core::{EnvKind, EnvRecipe};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::{cap_output, EnvStatus, Environment, ExecOutput};

pub const MAX_OUTPUT: usize = 48 * 1024;

pub struct HostEnv {
    recipe: EnvRecipe,
    files_dir: PathBuf,
}

impl HostEnv {
    pub fn new(recipe: EnvRecipe, files_dir: PathBuf) -> Self {
        Self { recipe, files_dir }
    }
}

/// Run a command in its own process group; kill the whole group on timeout
/// or cancellation so background children don't linger.
pub async fn run_process(mut cmd: Command, timeout: Duration, cancel: &CancellationToken) -> Result<ExecOutput> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let started = Instant::now();
    let mut child = cmd.spawn().context("failed to start process")?;
    let pid = child.id();
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let read_out = tokio::spawn(async move {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b).await;
        b
    });
    let read_err = tokio::spawn(async move {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b).await;
        b
    });
    let kill_group = |pid: Option<u32>| {
        if let Some(p) = pid {
            unsafe {
                libc::kill(-(p as i32), libc::SIGKILL);
            }
        }
    };
    let (status, timed_out) = tokio::select! {
        s = child.wait() => (s.ok(), false),
        _ = tokio::time::sleep(timeout) => { kill_group(pid); (child.wait().await.ok(), true) }
        _ = cancel.cancelled() => { kill_group(pid); (child.wait().await.ok(), true) }
    };
    // Background children may keep pipes open; don't wait forever for EOF.
    let grace = Duration::from_secs(2);
    let stdout = tokio::time::timeout(grace, read_out).await.ok().and_then(|r| r.ok()).unwrap_or_default();
    let stderr = tokio::time::timeout(grace, read_err).await.ok().and_then(|r| r.ok()).unwrap_or_default();
    Ok(ExecOutput {
        exit_code: status.and_then(|s| s.code()),
        stdout: cap_output(&stdout, MAX_OUTPUT),
        stderr: cap_output(&stderr, MAX_OUTPUT / 2),
        timed_out,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

#[async_trait]
impl Environment for HostEnv {
    fn name(&self) -> &str {
        &self.recipe.name
    }
    fn kind(&self) -> EnvKind {
        EnvKind::Host
    }
    fn recipe(&self) -> &EnvRecipe {
        &self.recipe
    }
    async fn status(&self) -> EnvStatus {
        EnvStatus::Ready
    }
    async fn start(&self, _: &(dyn Fn(String) + Send + Sync)) -> Result<()> {
        Ok(())
    }
    async fn stop(&self) -> Result<()> {
        Ok(())
    }
    async fn destroy(&self) -> Result<()> {
        Ok(())
    }
    async fn exec(&self, command: &str, cwd: &str, timeout: Duration, cancel: &CancellationToken) -> Result<ExecOutput> {
        let dir = kk_store_resolve(&self.files_dir, cwd)?;
        std::fs::create_dir_all(&dir)?;
        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(command).current_dir(&dir).env("WORKSPACE", &self.files_dir);
        run_process(cmd, timeout, cancel).await
    }
}

/// Minimal containment check for the working directory (the full sandbox
/// lives in kk-store; kept local to avoid a dependency cycle).
fn kk_store_resolve(root: &std::path::Path, rel: &str) -> Result<PathBuf> {
    let rel = rel.trim().trim_start_matches("/workspace").trim_start_matches('/');
    let p = root.join(rel);
    let canon_root = root.canonicalize()?;
    let mut probe = p.clone();
    while !probe.exists() {
        if !probe.pop() {
            break;
        }
    }
    let real = probe.canonicalize()?;
    anyhow::ensure!(real.starts_with(&canon_root), "working directory {rel:?} is outside the workspace");
    anyhow::ensure!(!rel.split('/').any(|c| c == ".."), "working directory may not contain '..'");
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(dir: &std::path::Path) -> HostEnv {
        let recipe: EnvRecipe = toml_recipe();
        HostEnv::new(recipe, dir.to_path_buf())
    }

    fn toml_recipe() -> EnvRecipe {
        serde_json::from_value(serde_json::json!({"name": "host", "kind": "host"})).unwrap()
    }

    #[tokio::test]
    async fn runs_and_times_out() {
        let d = tempfile::tempdir().unwrap();
        let e = env(d.path());
        let c = CancellationToken::new();
        let o = e.exec("echo hi; echo err >&2; exit 3", "", Duration::from_secs(10), &c).await.unwrap();
        assert_eq!(o.exit_code, Some(3));
        assert_eq!(o.stdout.trim(), "hi");
        assert_eq!(o.stderr.trim(), "err");
        let o = e.exec("sleep 30 & sleep 30", "", Duration::from_millis(300), &c).await.unwrap();
        assert!(o.timed_out);
        assert!(o.duration_ms < 5000);
        assert!(e.exec("true", "../", Duration::from_secs(5), &c).await.is_err());
    }
}
