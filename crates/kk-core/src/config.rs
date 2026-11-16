//! Configuration files that live in the home folder or a workspace.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------------------------------------------------------------- engines

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EngineKind {
    #[default]
    Chat,
    Tts,
    Image,
    Stt,
    Embeddings,
}

impl EngineKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EngineKind::Chat => "chat",
            EngineKind::Tts => "tts",
            EngineKind::Image => "image",
            EngineKind::Stt => "stt",
            EngineKind::Embeddings => "embeddings",
        }
    }
}

/// How tool calls are exchanged with a chat engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallMode {
    /// OpenAI `tools` / `tool_calls` fields. Text fallbacks are still parsed.
    #[default]
    Native,
    /// Tools are described in the system prompt and parsed from
    /// `<tool_call>{...}</tool_call>` blocks in the reply.
    Text,
}

/// One entry in the global engine catalog (`engines.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub id: String,
    #[serde(default)]
    pub kind: EngineKind,
    /// OpenAI-compatible base URL, e.g. `http://10.0.0.20:8000/v1`.
    pub base_url: String,
    /// Model name sent in requests.
    #[serde(default)]
    pub model: String,
    /// `secret:<name>` reference into the vault, `env:<VAR>`, or empty.
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_context_window")]
    pub context_window: u32,
    #[serde(default = "default_max_output")]
    pub max_output_tokens: u32,
    /// How many requests this engine may serve at once.
    #[serde(default = "one")]
    pub max_concurrent: u32,
    #[serde(default)]
    pub tool_call_mode: ToolCallMode,
    /// Optional `reasoning_effort` passed to the backend (low/medium/high).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Free text the lead agent reads when choosing engines.
    #[serde(default)]
    pub notes: String,
    /// `local`, `free`, `paid`… informational; `paid` engines can require approval.
    #[serde(default = "default_cost")]
    pub cost: String,
    /// Default voice for TTS engines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Extra JSON merged into every request body.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, toml::Value>,
}

fn default_context_window() -> u32 {
    32_768
}
fn default_max_output() -> u32 {
    8_192
}
fn one() -> u32 {
    1
}
fn default_cost() -> String {
    "local".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnginesFile {
    /// Engine used by roles that do not name one.
    #[serde(default)]
    pub default_chat: String,
    #[serde(default, rename = "engine")]
    pub engines: Vec<EngineConfig>,
}

impl EnginesFile {
    pub fn get(&self, id: &str) -> Option<&EngineConfig> {
        self.engines.iter().find(|e| e.id == id)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let mut seen = std::collections::HashSet::new();
        for e in &self.engines {
            anyhow::ensure!(crate::valid_ident(&e.id), "invalid engine id {:?}", e.id);
            anyhow::ensure!(seen.insert(&e.id), "duplicate engine id {:?}", e.id);
            anyhow::ensure!(
                e.base_url.starts_with("http://") || e.base_url.starts_with("https://"),
                "engine {}: base_url must start with http:// or https://",
                e.id
            );
            anyhow::ensure!(e.max_concurrent >= 1, "engine {}: max_concurrent must be >= 1", e.id);
        }
        if !self.default_chat.is_empty() {
            let d = self
                .get(&self.default_chat)
                .ok_or_else(|| anyhow::anyhow!("default_chat {:?} is not defined", self.default_chat))?;
            anyhow::ensure!(d.kind == EngineKind::Chat, "default_chat must be a chat engine");
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ roles

/// A role an agent can be given. Built-in roles ship with the app; the
/// library lives in `home/roles/`; a workspace keeps copies in `roles/`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Role {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Role-specific instructions appended to the base system prompt.
    #[serde(default)]
    pub prompt: String,
    /// Chat engine id; empty means the workspace/global default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Tool groups (`fs`, `shell`, `team`, …) or individual tool names.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Non-chat engines (tts, image…) this role may use through tools.
    #[serde(default)]
    pub engines: Vec<String>,
    /// Default environment for shell commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Run a completion self-check before a final reply counts as done.
    #[serde(default = "yes")]
    pub self_check: bool,
}

fn yes() -> bool {
    true
}

impl Role {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(crate::valid_ident(&self.id), "invalid role id {:?}", self.id);
        anyhow::ensure!(!self.name.trim().is_empty(), "role {} needs a name", self.id);
        Ok(())
    }
}

// -------------------------------------------------------------- templates

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Template {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Role given to the lead agent the user talks to.
    pub lead_role: String,
    /// Roles copied into the workspace at creation.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Agents created together with the lead (role ids).
    #[serde(default)]
    pub initial_agents: Vec<String>,
    #[serde(default)]
    pub settings: WorkspaceSettings,
    #[serde(default, rename = "environment")]
    pub environments: Vec<EnvRecipe>,
    /// Extra text added to every agent's prompt in this kind of workspace.
    #[serde(default)]
    pub workspace_prompt: String,
}

// ------------------------------------------------------------- workspaces

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    #[default]
    Auto,
    Ask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSettings {
    /// Chat engine for roles without one; empty means the global default.
    #[serde(default)]
    pub default_engine: String,
    /// Whether the lead may define roles / spawn agents without approval.
    #[serde(default)]
    pub team_policy: Policy,
    /// Whether shell commands need approval.
    #[serde(default)]
    pub command_policy: Policy,
    /// Whether engines with `cost = "paid"` need approval per use.
    #[serde(default = "ask")]
    pub paid_engine_policy: Policy,
    #[serde(default = "default_max_agents")]
    pub max_agents: u32,
    /// Engine ids agents may use. Empty means all.
    #[serde(default)]
    pub allowed_engines: Vec<String>,
    #[serde(default = "default_max_steps")]
    pub max_steps_per_run: u32,
    /// Agent-to-agent messages allowed without user input before pausing.
    #[serde(default = "default_autonomy")]
    pub autonomy_budget: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_environment: Option<String>,
}

fn ask() -> Policy {
    Policy::Ask
}
fn default_max_agents() -> u32 {
    8
}
fn default_max_steps() -> u32 {
    150
}
fn default_autonomy() -> u32 {
    60
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        toml::from_str("").expect("defaults")
    }
}

impl WorkspaceSettings {
    pub fn engine_allowed(&self, id: &str) -> bool {
        self.allowed_engines.is_empty() || self.allowed_engines.iter().any(|e| e == id)
    }
}

/// `workspace.toml`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceManifest {
    pub id: String,
    pub name: String,
    pub template: String,
    pub schema_version: u32,
    pub created_at: crate::Timestamp,
    #[serde(default)]
    pub workspace_prompt: String,
    #[serde(default)]
    pub settings: WorkspaceSettings,
}

// ----------------------------------------------------------- environments

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EnvKind {
    /// No command execution; file tools only.
    #[default]
    None,
    /// Commands run directly on the KodoKuru host inside the workspace
    /// `files/` folder. No isolation — for trusted/testing use only.
    Host,
    /// A Lima VM whose disk lives inside the workspace.
    Lima,
}

/// `envs/<name>/env.toml` — the recipe an environment is (re)built from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvRecipe {
    pub name: String,
    #[serde(default)]
    pub kind: EnvKind,
    #[serde(default)]
    pub description: String,
    /// Lima base template, e.g. `template://ubuntu-lts`.
    #[serde(default = "default_image")]
    pub image: String,
    #[serde(default = "default_cpus")]
    pub cpus: u32,
    #[serde(default = "default_memory")]
    pub memory: String,
    #[serde(default = "default_disk")]
    pub disk: String,
    /// Shell script run as root on first boot (and on rebuild).
    #[serde(default)]
    pub provision: String,
    /// Keep the VM disk inside the workspace (exact state travels with it).
    #[serde(default = "yes")]
    pub keep_disk: bool,
    /// Allow outbound internet from the guest.
    #[serde(default = "yes")]
    pub network: bool,
}

fn default_image() -> String {
    "template://ubuntu-lts".into()
}
fn default_cpus() -> u32 {
    2
}
fn default_memory() -> String {
    "4GiB".into()
}
fn default_disk() -> String {
    "40GiB".into()
}

impl EnvRecipe {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(crate::valid_ident(&self.name), "invalid environment name {:?}", self.name);
        anyhow::ensure!(self.cpus >= 1 && self.cpus <= 128, "cpus out of range");
        Ok(())
    }
}

// ------------------------------------------------------------ home config

/// `kodokuru.toml` in the home folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeSettings {
    pub schema_version: u32,
    pub home_id: String,
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "yes")]
    pub tls: bool,
    /// Check GitHub releases for updates at startup.
    #[serde(default = "yes")]
    pub auto_update: bool,
    #[serde(default = "default_repo")]
    pub update_repo: String,
}

fn default_bind() -> String {
    "0.0.0.0".into()
}
fn default_port() -> u16 {
    7443
}
fn default_repo() -> String {
    "effishen/KodoKuru".into()
}

/// `machines/<machine-id>.toml` — settings that differ per host machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineSettings {
    pub machine_id: String,
    #[serde(default)]
    pub hostname: String,
    /// Upper bound on total VM CPUs this machine may allocate.
    #[serde(default)]
    pub max_vm_cpus: u32,
    /// Upper bound on concurrently running VMs.
    #[serde(default = "default_max_vms")]
    pub max_running_vms: u32,
    /// Override port for this machine (0 = use home setting).
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub last_seen: Option<crate::Timestamp>,
}

fn default_max_vms() -> u32 {
    4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_parse_and_validate() {
        let f: EnginesFile = toml::from_str(
            r#"
            default_chat = "local"
            [[engine]]
            id = "local"
            base_url = "http://10.0.0.20:8000/v1"
            model = "qwen"
            [[engine]]
            id = "tts"
            kind = "tts"
            base_url = "https://example.com/v1"
            cost = "paid"
            "#,
        )
        .unwrap();
        f.validate().unwrap();
        assert_eq!(f.get("local").unwrap().max_concurrent, 1);
        assert_eq!(f.get("tts").unwrap().kind, EngineKind::Tts);
    }

    #[test]
    fn bad_default_rejected() {
        let f: EnginesFile = toml::from_str("default_chat = \"nope\"").unwrap();
        assert!(f.validate().is_err());
    }

    #[test]
    fn settings_defaults() {
        let s = WorkspaceSettings::default();
        assert_eq!(s.max_agents, 8);
        assert_eq!(s.paid_engine_policy, Policy::Ask);
        assert!(s.engine_allowed("anything"));
    }
}
