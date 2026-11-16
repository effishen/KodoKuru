//! Environments agents run commands in. Each workspace owns its
//! environments; recipes (`env.toml`) live in the workspace so a VM can be
//! rebuilt anywhere, and Lima VM disks can live inside the workspace too.

pub mod doctor;
pub mod host;
pub mod lima;

use anyhow::Result;
use async_trait::async_trait;
use kk_core::{EnvKind, EnvRecipe};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EnvStatus {
    /// Kind `none`: nothing to start.
    NotApplicable,
    /// Always available (host).
    Ready,
    /// VM not created yet (will be built from the recipe on start).
    Absent,
    Stopped,
    Starting,
    Running,
    Broken { message: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub duration_ms: u64,
}

impl ExecOutput {
    /// Compact text for the model.
    pub fn render(&self) -> String {
        let mut s = String::new();
        match (self.timed_out, self.exit_code) {
            (true, _) => s.push_str("[timed out — process killed]\n"),
            (false, Some(c)) => s.push_str(&format!("[exit code {c}]\n")),
            (false, None) => s.push_str("[terminated by signal]\n"),
        }
        if !self.stdout.is_empty() {
            s.push_str("--- stdout ---\n");
            s.push_str(&self.stdout);
            if !self.stdout.ends_with('\n') {
                s.push('\n');
            }
        }
        if !self.stderr.is_empty() {
            s.push_str("--- stderr ---\n");
            s.push_str(&self.stderr);
        }
        if self.stdout.is_empty() && self.stderr.is_empty() {
            s.push_str("(no output)");
        }
        s
    }
}

#[async_trait]
pub trait Environment: Send + Sync {
    fn name(&self) -> &str;
    fn kind(&self) -> EnvKind;
    fn recipe(&self) -> &EnvRecipe;
    async fn status(&self) -> EnvStatus;
    /// Create (from the recipe) if needed and start. `progress` gets log lines.
    async fn start(&self, progress: &(dyn Fn(String) + Send + Sync)) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    /// Delete the VM so the next start rebuilds it from the recipe.
    async fn destroy(&self) -> Result<()>;
    /// Run a shell command. `cwd` is relative to the workspace files.
    async fn exec(&self, command: &str, cwd: &str, timeout: Duration, cancel: &CancellationToken) -> Result<ExecOutput>;
}

pub struct NoneEnv {
    recipe: EnvRecipe,
}

#[async_trait]
impl Environment for NoneEnv {
    fn name(&self) -> &str {
        &self.recipe.name
    }
    fn kind(&self) -> EnvKind {
        EnvKind::None
    }
    fn recipe(&self) -> &EnvRecipe {
        &self.recipe
    }
    async fn status(&self) -> EnvStatus {
        EnvStatus::NotApplicable
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
    async fn exec(&self, _: &str, _: &str, _: Duration, _: &CancellationToken) -> Result<ExecOutput> {
        anyhow::bail!(
            "environment {:?} has no command execution (kind = none); use file tools, or ask for a VM environment",
            self.recipe.name
        )
    }
}

/// Everything an environment needs to know about where it lives.
#[derive(Debug, Clone)]
pub struct EnvContext {
    pub workspace_id: String,
    pub files_dir: PathBuf,
    /// `<workspace>/envs/<name>`
    pub env_dir: PathBuf,
    pub cache_dir: PathBuf,
}

pub fn build(recipe: EnvRecipe, ctx: EnvContext) -> Arc<dyn Environment> {
    match recipe.kind {
        EnvKind::None => Arc::new(NoneEnv { recipe }),
        EnvKind::Host => Arc::new(host::HostEnv::new(recipe, ctx.files_dir)),
        EnvKind::Lima => Arc::new(lima::LimaEnv::new(recipe, ctx)),
    }
}

/// Keep the head and tail of long output so the model sees both the start
/// and the error at the end.
pub fn cap_output(bytes: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    if s.len() <= max {
        return s.into_owned();
    }
    let half = max / 2;
    let mut head_end = half;
    while !s.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = s.len() - half;
    while !s.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!(
        "{}\n… [{} bytes omitted] …\n{}",
        &s[..head_end],
        tail_start - head_end,
        &s[tail_start..]
    )
}

pub(crate) fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| is_executable(p))
}

pub(crate) fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// Parse sizes like `8GiB`, `512MiB`, `4G` into GiB (float) for limactl.
pub fn parse_gib(s: &str) -> Option<f64> {
    let s = s.trim();
    let idx = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let n: f64 = s[..idx].parse().ok()?;
    let unit = s[idx..].trim().to_ascii_lowercase();
    Some(match unit.as_str() {
        "" | "g" | "gb" | "gib" => n,
        "m" | "mb" | "mib" => n / 1024.0,
        "t" | "tb" | "tib" => n * 1024.0,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_output() {
        let long = "a".repeat(100) + &"b".repeat(100);
        let c = cap_output(long.as_bytes(), 40);
        assert!(c.starts_with("aaaa") && c.ends_with("bbbb") && c.contains("omitted"));
        assert_eq!(cap_output(b"short", 40), "short");
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_gib("8GiB"), Some(8.0));
        assert_eq!(parse_gib("512MiB"), Some(0.5));
        assert_eq!(parse_gib("4"), Some(4.0));
        assert_eq!(parse_gib("lots"), None);
    }
}
