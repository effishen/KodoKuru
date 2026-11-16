//! `run_command`: shell commands in one of the workspace environments.

use anyhow::{bail, Result};
use async_trait::async_trait;
use kk_core::{EnvKind, Policy};
use kk_env::EnvStatus;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

use super::{opt_s, opt_u, s, Tool, ToolCtx};

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(RunCommand)]
}

/// Which environment a command runs in: explicit, role default, workspace
/// default, or the only one that can run commands.
pub(crate) fn pick_env(ctx: &ToolCtx, explicit: Option<&str>) -> Result<String> {
    let recipes = ctx.ws.env_recipes()?;
    let runnable: Vec<_> = recipes.iter().filter(|r| r.kind != EnvKind::None).collect();
    let wanted = explicit
        .map(str::to_string)
        .or_else(|| ctx.role.environment.clone())
        .or_else(|| ctx.ws.settings().default_environment.clone());
    if let Some(w) = wanted {
        if recipes.iter().any(|r| r.name == w) {
            return Ok(w);
        }
        if explicit.is_some() {
            bail!(
                "no environment {w:?}; available: {}",
                recipes.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ")
            );
        }
    }
    match runnable.as_slice() {
        [one] => Ok(one.name.clone()),
        [] => bail!("this workspace has no environment that can run commands; ask the lead (or the user) to define one with env_define"),
        many => bail!(
            "several environments exist ({}); pass `environment`",
            many.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ")
        ),
    }
}

struct RunCommand;
#[async_trait]
impl Tool for RunCommand {
    fn name(&self) -> &'static str {
        "run_command"
    }
    fn group(&self) -> &'static str {
        "shell"
    }
    fn description(&self) -> String {
        "Run a bash command in an environment (VM or host) with the workspace files as the working directory. Returns exit code, stdout and stderr (long output is shortened in the middle). Non-interactive: never wait for input. For long-running servers, start them in the background with nohup and log to a file.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["command"], "properties": {
            "command": {"type": "string"},
            "environment": {"type": "string", "description": "Environment name (default: your role's or the workspace's)"},
            "cwd": {"type": "string", "description": "Working directory relative to the workspace files"},
            "timeout_secs": {"type": "integer", "description": "Default 120, max 3600"}
        }})
    }
    async fn approval(&self, ctx: &ToolCtx, a: &Value) -> Option<String> {
        let env = pick_env(ctx, opt_s(a, "environment")).unwrap_or_else(|_| "?".into());
        if ctx.ws.settings().command_policy == Policy::Ask {
            return Some(format!("Run in `{env}`: {}", a.get("command").and_then(|c| c.as_str()).unwrap_or("")));
        }
        None
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let command = s(&a, "command")?;
        let name = pick_env(ctx, opt_s(&a, "environment"))?;
        let env = ctx.ws.env(&name).await?;
        match env.status().await {
            EnvStatus::Absent | EnvStatus::Stopped => {
                ctx.ws.notice(Some(&ctx.agent.id), "info", format!("Starting environment `{name}`…"));
                ctx.ws.start_env(&name).await?;
            }
            EnvStatus::Broken { message } => bail!("environment {name} is broken: {message}"),
            _ => {}
        }
        let timeout = Duration::from_secs(opt_u(&a, "timeout_secs").unwrap_or(120).clamp(1, 3600));
        let out = env.exec(command, opt_s(&a, "cwd").unwrap_or(""), timeout, &ctx.cancel).await?;
        Ok(out.render())
    }
}
