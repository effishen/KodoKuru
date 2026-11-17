//! Environment management tools.

use anyhow::Result;
use async_trait::async_trait;
use kk_core::{EnvKind, EnvRecipe, Policy};
use serde_json::{json, Value};
use std::sync::Arc;

use super::{opt_b, opt_s, opt_u, s, Tool, ToolCtx};

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(EnvList), Arc::new(EnvStart), Arc::new(EnvStop), Arc::new(EnvDefine)]
}

struct EnvList;
#[async_trait]
impl Tool for EnvList {
    fn name(&self) -> &'static str {
        "env_list"
    }
    fn group(&self) -> &'static str {
        "env"
    }
    fn description(&self) -> String {
        "List this workspace's environments with their recipe and current status.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    async fn run(&self, ctx: &ToolCtx, _a: Value) -> Result<String> {
        let st = ctx.ws.env_statuses().await;
        if st.is_empty() {
            return Ok("No environments defined. Use env_define to create one.".into());
        }
        Ok(st
            .iter()
            .map(|(r, s)| {
                format!(
                    "- {} kind={:?} status={} cpus={} memory={} disk={} keep_disk={} — {}",
                    r.name,
                    r.kind,
                    serde_json::to_value(s).ok().and_then(|v| v["state"].as_str().map(str::to_string)).unwrap_or_default(),
                    r.cpus,
                    r.memory,
                    r.disk,
                    r.keep_disk,
                    r.description
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

struct EnvStart;
#[async_trait]
impl Tool for EnvStart {
    fn name(&self) -> &'static str {
        "env_start"
    }
    fn group(&self) -> &'static str {
        "env"
    }
    fn description(&self) -> String {
        "Start an environment (creating the VM from its recipe the first time, which can take several minutes).".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}}})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let name = s(&a, "name")?;
        ctx.ws.start_env(name).await?;
        Ok(format!("Environment {name} is running."))
    }
}

struct EnvStop;
#[async_trait]
impl Tool for EnvStop {
    fn name(&self) -> &'static str {
        "env_stop"
    }
    fn group(&self) -> &'static str {
        "env"
    }
    fn description(&self) -> String {
        "Stop a running environment.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}}})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let name = s(&a, "name")?;
        ctx.ws.stop_env(name).await?;
        Ok(format!("Environment {name} stopped."))
    }
}

struct EnvDefine;
#[async_trait]
impl Tool for EnvDefine {
    fn name(&self) -> &'static str {
        "env_define"
    }
    fn group(&self) -> &'static str {
        "env"
    }
    fn description(&self) -> String {
        "Create or change an environment recipe. kind=lima is an isolated Linux VM (Ubuntu by default) built from the recipe, with the workspace files at /workspace. Put every package the work needs in `provision` so the VM can be rebuilt anywhere. Changing an existing VM's recipe takes effect when it is rebuilt.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["name", "kind"], "properties": {
            "name": {"type": "string"},
            "kind": {"type": "string", "enum": ["lima", "host", "none"]},
            "description": {"type": "string"},
            "image": {"type": "string", "description": "Lima template, default template:ubuntu-lts"},
            "cpus": {"type": "integer"},
            "memory": {"type": "string", "description": "e.g. 4GiB"},
            "disk": {"type": "string", "description": "e.g. 40GiB"},
            "provision": {"type": "string", "description": "bash script run as root on first boot"},
            "network": {"type": "boolean", "description": "intended outbound network access (not enforced yet)"}
        }})
    }
    async fn approval(&self, ctx: &ToolCtx, a: &Value) -> Option<String> {
        let kind = a.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        // Host environments are unisolated: always ask.
        if kind == "host" || ctx.ws.settings().team_policy == Policy::Ask {
            return Some(format!("Define {kind} environment `{}`", a.get("name").and_then(|v| v.as_str()).unwrap_or("?")));
        }
        None
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let mut r: EnvRecipe = ctx
            .ws
            .env_recipes()?
            .into_iter()
            .find(|r| r.name == s(&a, "name").unwrap_or(""))
            .unwrap_or_else(|| serde_json::from_value(json!({"name": a["name"]})).expect("recipe defaults"));
        r.kind = match s(&a, "kind")? {
            "lima" => EnvKind::Lima,
            "host" => EnvKind::Host,
            _ => EnvKind::None,
        };
        if let Some(v) = opt_s(&a, "description") {
            r.description = v.into();
        }
        if let Some(v) = opt_s(&a, "image") {
            r.image = v.into();
        }
        if let Some(v) = opt_u(&a, "cpus") {
            let max = ctx.ws.rt.home.machine.max_vm_cpus.max(1) as u64;
            r.cpus = v.clamp(1, max) as u32;
        }
        if let Some(v) = opt_s(&a, "memory") {
            r.memory = v.into();
        }
        if let Some(v) = opt_s(&a, "disk") {
            r.disk = v.into();
        }
        if let Some(v) = opt_s(&a, "provision") {
            r.provision = v.into();
        }
        if let Some(v) = opt_b(&a, "network") {
            r.network = v;
        }
        ctx.ws.define_env(r.clone()).await?;
        Ok(format!("Environment `{}` saved ({:?}). It is created on first start.", r.name, r.kind))
    }
}
