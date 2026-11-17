//! Lima VM environments, driven through `limactl`.
//!
//! Portability: when `keep_disk = true` the Lima instance directory (disk
//! included) lives in `<workspace>/envs/<name>/lima/`. Lima uses unix sockets
//! under `LIMA_HOME`, whose paths must stay short, so `LIMA_HOME` points at a
//! short symlink in the cache. On start, the mount location is re-pointed at
//! the current `files/` folder, so a workspace copied to another machine
//! keeps working. With `keep_disk = false` the instance lives in the cache
//! and is rebuilt from the recipe on a new machine.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use kk_core::{EnvKind, EnvRecipe};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{host::run_process, parse_gib, EnvContext, EnvStatus, Environment, ExecOutput};

/// Lima release installed automatically when none is found.
pub const LIMA_VERSION: &str = "2.2.1";

pub struct LimaEnv {
    recipe: EnvRecipe,
    ctx: EnvContext,
    instance: String,
    busy: Mutex<()>,
}

/// Find `limactl`: PATH first, then the managed copy in the cache.
pub fn limactl_path(cache: &Path) -> Option<PathBuf> {
    crate::which("limactl").or_else(|| {
        let p = managed_dir(cache).join("bin").join("limactl");
        crate::is_executable(&p).then_some(p)
    })
}

pub fn managed_dir(cache: &Path) -> PathBuf {
    cache.join("tools").join(format!("lima-{LIMA_VERSION}"))
}

impl LimaEnv {
    pub fn new(recipe: EnvRecipe, ctx: EnvContext) -> Self {
        let short: String = ctx.workspace_id.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
        let instance = format!("kk-{short}-{}", recipe.name.replace('_', "-"));
        Self { recipe, ctx, instance, busy: Mutex::new(()) }
    }

    fn limactl(&self) -> Result<PathBuf> {
        limactl_path(&self.ctx.cache_dir).context(
            "limactl not found. Run `kodokuru doctor --fix` to install Lima, or install it yourself",
        )
    }

    /// The real directory holding Lima's instance data.
    fn data_dir(&self) -> PathBuf {
        if self.recipe.keep_disk {
            self.ctx.env_dir.join("lima")
        } else {
            let short: String = self.ctx.workspace_id.chars().take(8).collect();
            self.ctx.cache_dir.join("lima-data").join(short)
        }
    }

    /// Short `LIMA_HOME` path (a symlink in the cache to [`data_dir`]).
    fn lima_home(&self) -> Result<PathBuf> {
        let data = self.data_dir();
        std::fs::create_dir_all(&data)?;
        let short: String = self.ctx.workspace_id.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
        let link = self.ctx.cache_dir.join("lh").join(short);
        std::fs::create_dir_all(link.parent().unwrap())?;
        match std::fs::read_link(&link) {
            Ok(target) if target == data => {}
            Ok(_) => {
                std::fs::remove_file(&link)?;
                std::os::unix::fs::symlink(&data, &link)?;
            }
            Err(_) => {
                let _ = std::fs::remove_file(&link);
                std::os::unix::fs::symlink(&data, &link)?;
            }
        }
        Ok(link)
    }

    fn cmd(&self) -> Result<Command> {
        let mut c = Command::new(self.limactl()?);
        c.env("LIMA_HOME", self.lima_home()?);
        c.env("LIMA_INSTANCE", &self.instance);
        Ok(c)
    }

    async fn simple(&self, args: &[&str], timeout: Duration) -> Result<ExecOutput> {
        let mut c = self.cmd()?;
        c.args(args);
        run_process(c, timeout, &CancellationToken::new()).await
    }

    async fn run_logged(&self, mut c: Command, progress: &(dyn Fn(String) + Send + Sync), timeout: Duration) -> Result<()> {
        c.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = c.spawn().context("start limactl")?;
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let tx2 = tx.clone();
        tokio::spawn(async move {
            let mut l = BufReader::new(out).lines();
            while let Ok(Some(line)) = l.next_line().await {
                let _ = tx.send(line);
            }
        });
        tokio::spawn(async move {
            let mut l = BufReader::new(err).lines();
            while let Ok(Some(line)) = l.next_line().await {
                let _ = tx2.send(line);
            }
        });
        let mut tail: Vec<String> = Vec::new();
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let status = loop {
            tokio::select! {
                Some(line) = rx.recv() => {
                    progress(line.clone());
                    tail.push(line);
                    if tail.len() > 30 { tail.remove(0); }
                }
                s = child.wait() => break s?,
                _ = &mut deadline => {
                    let _ = child.kill().await;
                    bail!("limactl timed out after {}s", timeout.as_secs());
                }
            }
        };
        while let Ok(line) = rx.try_recv() {
            progress(line.clone());
            tail.push(line);
        }
        if !status.success() {
            bail!("limactl failed ({status}):\n{}", tail.join("\n"));
        }
        Ok(())
    }

    async fn instance_status(&self) -> Result<Option<String>> {
        let o = self.simple(&["list", "--json"], Duration::from_secs(30)).await?;
        if o.exit_code != Some(0) {
            bail!("limactl list failed: {}", o.stderr.trim());
        }
        for line in o.stdout.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if v["name"] == self.instance.as_str() {
                    return Ok(Some(v["status"].as_str().unwrap_or("Unknown").to_string()));
                }
            }
        }
        Ok(None)
    }

    fn mount_expr(&self) -> String {
        format!(
            ".mounts = [{{\"location\": {}, \"mountPoint\": \"/workspace\", \"writable\": true}}]",
            serde_json::to_string(&self.ctx.files_dir.to_string_lossy()).unwrap()
        )
    }

    async fn create(&self, progress: &(dyn Fn(String) + Send + Sync)) -> Result<()> {
        let cpus = self.recipe.cpus.to_string();
        let mem = parse_gib(&self.recipe.memory).context("bad memory size")?;
        let disk = parse_gib(&self.recipe.disk).context("bad disk size")?;
        let mut c = self.cmd()?;
        c.args(["create", "--tty=false", "--name", &self.instance])
            .arg(format!("--cpus={cpus}"))
            .arg(format!("--memory={mem}"))
            .arg(format!("--disk={disk}"));
        // limactl takes a single --set yq expression; chain edits with `|`.
        let mut expr = format!("{} | .containerd.system = false | .containerd.user = false", self.mount_expr());
        if !self.recipe.provision.trim().is_empty() {
            let script = format!("#!/bin/bash\nset -eux -o pipefail\n{}", self.recipe.provision);
            expr.push_str(&format!(
                " | .provision += [{{\"mode\": \"system\", \"script\": {}}}]",
                serde_json::to_string(&script)?
            ));
        }
        c.arg("--set").arg(expr).arg(&self.recipe.image);
        progress(format!("creating VM {} from {}", self.instance, self.recipe.image));
        self.run_logged(c, progress, Duration::from_secs(30 * 60)).await
    }
}

#[async_trait]
impl Environment for LimaEnv {
    fn name(&self) -> &str {
        &self.recipe.name
    }
    fn kind(&self) -> EnvKind {
        EnvKind::Lima
    }
    fn recipe(&self) -> &EnvRecipe {
        &self.recipe
    }

    async fn status(&self) -> EnvStatus {
        if self.busy.try_lock().is_err() {
            return EnvStatus::Starting;
        }
        match self.instance_status().await {
            Ok(None) => EnvStatus::Absent,
            Ok(Some(s)) if s == "Running" => EnvStatus::Running,
            Ok(Some(s)) if s == "Stopped" => EnvStatus::Stopped,
            Ok(Some(s)) => EnvStatus::Broken { message: format!("Lima reports {s}") },
            Err(e) => EnvStatus::Broken { message: format!("{e:#}") },
        }
    }

    async fn start(&self, progress: &(dyn Fn(String) + Send + Sync)) -> Result<()> {
        let _g = self.busy.lock().await;
        match self.instance_status().await? {
            None => self.create(progress).await?,
            Some(s) if s == "Running" => return Ok(()),
            Some(_) => {
                // Re-point the shared folder in case the workspace moved.
                let mut c = self.cmd()?;
                c.args(["edit", "--tty=false", "--set"]).arg(self.mount_expr()).arg(&self.instance);
                self.run_logged(c, progress, Duration::from_secs(120)).await?;
            }
        }
        let mut c = self.cmd()?;
        c.args(["start", "--tty=false", &self.instance]);
        progress(format!("starting VM {}", self.instance));
        self.run_logged(c, progress, Duration::from_secs(30 * 60)).await
    }

    async fn stop(&self) -> Result<()> {
        let _g = self.busy.lock().await;
        if self.instance_status().await?.as_deref() != Some("Running") {
            return Ok(());
        }
        let o = self.simple(&["stop", &self.instance], Duration::from_secs(180)).await?;
        if o.exit_code != Some(0) {
            bail!("limactl stop failed: {}", o.stderr.trim());
        }
        Ok(())
    }

    async fn destroy(&self) -> Result<()> {
        let _g = self.busy.lock().await;
        let o = self.simple(&["delete", "--force", &self.instance], Duration::from_secs(180)).await?;
        if o.exit_code != Some(0) && !o.stderr.contains("not found") && !o.stderr.contains("not exist") {
            bail!("limactl delete failed: {}", o.stderr.trim());
        }
        Ok(())
    }

    async fn exec(&self, command: &str, cwd: &str, timeout: Duration, cancel: &CancellationToken) -> Result<ExecOutput> {
        if self.instance_status().await?.as_deref() != Some("Running") {
            bail!(
                "environment {:?} is not running; start it with env_start first",
                self.recipe.name
            );
        }
        let cwd = cwd.trim().trim_start_matches("/workspace").trim_matches('/');
        anyhow::ensure!(!cwd.split('/').any(|c| c == ".."), "cwd may not contain '..'");
        let workdir = if cwd.is_empty() { "/workspace".to_string() } else { format!("/workspace/{cwd}") };
        let secs = timeout.as_secs().max(1);
        let mut c = self.cmd()?;
        c.args(["shell", "--workdir", &workdir, &self.instance, "--"])
            .args(["timeout", "--kill-after=5", &secs.to_string(), "bash", "-lc", command]);
        // Outer timeout is a safety net in case the guest hangs.
        run_process(c, timeout + Duration::from_secs(15), cancel).await
    }
}

/// Download and install the pinned Lima release into the cache, verifying
/// the official SHA256SUMS. Returns the install directory.
pub async fn install_managed(cache: &Path, progress: &(dyn Fn(String) + Send + Sync)) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        a => bail!("no Lima build for architecture {a}"),
    };
    let os = match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "Darwin",
        o => bail!("no Lima build for {o}"),
    };
    let asset = format!("lima-{LIMA_VERSION}-{os}-{arch}.tar.gz");
    let base = format!("https://github.com/lima-vm/lima/releases/download/v{LIMA_VERSION}");
    let http = reqwest::Client::builder().timeout(Duration::from_secs(600)).build()?;
    progress(format!("downloading {base}/SHA256SUMS"));
    let sums = http.get(format!("{base}/SHA256SUMS")).send().await?.error_for_status()?.text().await?;
    let expected = sums
        .lines()
        .find_map(|l| {
            let mut it = l.split_whitespace();
            let h = it.next()?;
            let name = it.next()?.trim_start_matches('*');
            (name == asset).then(|| h.to_string())
        })
        .with_context(|| format!("{asset} not listed in SHA256SUMS"))?;
    progress(format!("downloading {asset}"));
    let bytes = http.get(format!("{base}/{asset}")).send().await?.error_for_status()?.bytes().await?;
    let got = hex::encode(Sha256::digest(&bytes));
    if got != expected {
        bail!("checksum mismatch for {asset}: expected {expected}, got {got}");
    }
    let dest = managed_dir(cache);
    let staging = dest.with_extension("staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let gz = flate2::read::GzDecoder::new(&bytes[..]);
    tar::Archive::new(gz).unpack(&staging).context("unpack Lima")?;
    anyhow::ensure!(staging.join("bin/limactl").exists(), "archive has no bin/limactl");
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&staging, &dest)?;
    progress(format!("installed Lima {LIMA_VERSION} into {}", dest.display()));
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names_and_mounts() {
        let recipe: EnvRecipe = serde_json::from_value(serde_json::json!({"name": "dev_1", "kind": "lima"})).unwrap();
        let ctx = EnvContext {
            workspace_id: "1234abcd-5678".into(),
            files_dir: "/data/ws/files".into(),
            env_dir: "/data/ws/envs/dev_1".into(),
            cache_dir: "/tmp/c".into(),
        };
        let e = LimaEnv::new(recipe, ctx);
        assert_eq!(e.instance, "kk-1234abcd-dev-1");
        assert!(e.mount_expr().contains("\"/data/ws/files\""));
        assert_eq!(e.data_dir(), PathBuf::from("/data/ws/envs/dev_1/lima"));
    }
}
