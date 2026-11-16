//! A live workspace: its agents, their runs, environments, approvals and the
//! message routing between agents.

use anyhow::{bail, Context, Result};
use kk_core::*;
use kk_env::{EnvContext, EnvStatus, Environment};
use kk_store::{Store, WorkspaceDir};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::runtime::Runtime;
use crate::Live;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    None,
    /// The user pressed stop.
    User,
    /// Vault locked: resume on unlock.
    Lock,
    /// Process shutting down: resume next start.
    Shutdown,
}

#[derive(Clone)]
pub struct RunCtl {
    pub cancel: CancellationToken,
    pub reason: Arc<Mutex<StopReason>>,
    /// New input arrived while the run was active.
    pub again: Arc<AtomicBool>,
}

impl RunCtl {
    fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            reason: Arc::new(Mutex::new(StopReason::None)),
            again: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn stop_reason(&self) -> StopReason {
        *self.reason.lock().unwrap()
    }
}

pub struct PendingApproval {
    pub agent_id: String,
    pub tool: String,
    pub summary: String,
    tx: oneshot::Sender<(bool, String)>,
}

pub struct Workspace {
    pub rt: Arc<Runtime>,
    dir: WorkspaceDir,
    manifest: RwLock<WorkspaceManifest>,
    envs: tokio::sync::Mutex<HashMap<String, Arc<dyn Environment>>>,
    runs: Mutex<HashMap<String, RunCtl>>,
    approvals: Mutex<HashMap<String, PendingApproval>>,
    /// Agent-to-agent messages since the user last spoke.
    autonomy: AtomicU32,
}

impl Workspace {
    pub fn new(rt: Arc<Runtime>, dir: WorkspaceDir) -> Arc<Workspace> {
        let manifest = dir.manifest.clone();
        Arc::new(Workspace {
            rt,
            dir,
            manifest: RwLock::new(manifest),
            envs: Default::default(),
            runs: Default::default(),
            approvals: Default::default(),
            autonomy: AtomicU32::new(0),
        })
    }

    pub fn id(&self) -> &str {
        &self.dir.manifest.id
    }
    pub fn manifest(&self) -> WorkspaceManifest {
        self.manifest.read().unwrap().clone()
    }
    pub fn name(&self) -> String {
        self.manifest.read().unwrap().name.clone()
    }
    pub fn created_at(&self) -> Timestamp {
        self.dir.manifest.created_at
    }
    pub fn settings(&self) -> WorkspaceSettings {
        self.manifest.read().unwrap().settings.clone()
    }
    pub fn root(&self) -> &std::path::Path {
        &self.dir.root
    }
    pub fn files_dir(&self) -> PathBuf {
        self.dir.files_dir()
    }
    pub fn store(&self) -> &Store {
        &self.dir.store
    }
    pub fn dir(&self) -> &WorkspaceDir {
        &self.dir
    }

    pub fn update_manifest(&self, f: impl FnOnce(&mut WorkspaceManifest)) -> Result<()> {
        let mut m = self.manifest.write().unwrap();
        f(&mut m);
        kk_home::fsutil::write_toml(&self.dir.root.join("workspace.toml"), &*m)
    }

    // ------------------------------------------------------------ events

    fn visibility(&self, agent_id: Option<&str>, kind: &EventKind) -> Visibility {
        let is_lead = agent_id
            .and_then(|a| self.store().agent(a).ok().flatten())
            .map(|a| a.is_lead)
            .unwrap_or(false);
        let public = match kind {
            EventKind::Assistant { .. } => is_lead,
            EventKind::Message { from: Sender::User, .. } => true,
            EventKind::AgentCreated { .. } | EventKind::WorkspaceCreated { .. } | EventKind::Notice { .. } => true,
            EventKind::ApprovalRequested { .. } | EventKind::ApprovalResolved { .. } => true,
            EventKind::RunState { .. } | EventKind::Checklist { .. } | EventKind::TaskUpsert { .. } => true,
            _ => false,
        };
        if public {
            Visibility::Public
        } else {
            Visibility::Internal
        }
    }

    /// Append an event and push it to connected UIs.
    pub fn emit(&self, agent_id: Option<&str>, kind: EventKind) -> Result<Event> {
        let visibility = self.visibility(agent_id, &kind);
        let ev = self.store().append(NewEvent { agent_id: agent_id.map(str::to_string), visibility, kind })?;
        self.rt.publish(Some(self.id()), Live::Event { event: Box::new(ev.clone()) });
        Ok(ev)
    }

    pub fn notice(&self, agent_id: Option<&str>, level: &str, text: impl Into<String>) {
        let _ = self.emit(agent_id, EventKind::Notice { level: level.into(), text: text.into() });
    }

    pub fn set_run_state(&self, agent_id: &str, state: RunState) {
        if self.store().run_state(agent_id).ok().flatten().as_ref() == Some(&state) {
            return;
        }
        let _ = self.emit(Some(agent_id), EventKind::RunState { state });
    }

    // ------------------------------------------------------------ agents

    pub async fn initialize(self: &Arc<Self>, template: &Template) -> Result<()> {
        self.emit(None, EventKind::WorkspaceCreated { name: self.name(), template: template.id.clone() })?;
        self.create_agent(&template.lead_role, None, Sender::User, true)?;
        for r in &template.initial_agents {
            self.create_agent(r, None, Sender::User, false)?;
        }
        Ok(())
    }

    pub fn agents(&self) -> Result<Vec<AgentInfo>> {
        self.store().agents()
    }

    pub fn lead(&self) -> Result<AgentInfo> {
        self.agents()?.into_iter().find(|a| a.is_lead).context("workspace has no lead agent")
    }

    /// Find an agent by id or (case-insensitive) name.
    pub fn find_agent(&self, key: &str) -> Result<AgentInfo> {
        let key = key.trim();
        let agents = self.agents()?;
        agents
            .iter()
            .find(|a| a.id == key)
            .or_else(|| agents.iter().find(|a| a.name.eq_ignore_ascii_case(key)))
            .or_else(|| {
                let m: Vec<_> = agents.iter().filter(|a| a.role_id.eq_ignore_ascii_case(key)).collect();
                if m.len() == 1 {
                    Some(m[0])
                } else {
                    None
                }
            })
            .cloned()
            .with_context(|| {
                format!(
                    "no agent {key:?}; agents are: {}",
                    agents.iter().map(|a| format!("{} ({})", a.name, a.id)).collect::<Vec<_>>().join(", ")
                )
            })
    }

    /// The role for an id: the workspace copy, else the library (copied in).
    pub fn ensure_role(&self, role_id: &str) -> Result<Role> {
        if let Some(r) = self.dir.role(role_id)? {
            return Ok(r);
        }
        let r = self
            .rt
            .home
            .role(role_id)?
            .with_context(|| format!("unknown role {role_id:?}; define it with define_role first"))?;
        self.dir.save_role(&r)?;
        Ok(r)
    }

    pub fn roles(&self) -> Result<Vec<Role>> {
        self.dir.roles()
    }

    pub fn save_role(&self, role: &Role, by: Sender) -> Result<()> {
        self.dir.save_role(role)?;
        self.emit(None, EventKind::RoleDefined { role: role.clone(), by })?;
        Ok(())
    }

    pub fn create_agent(&self, role_id: &str, name: Option<&str>, by: Sender, is_lead: bool) -> Result<AgentInfo> {
        let role = self.ensure_role(role_id)?;
        let agents = self.agents()?;
        let max = self.settings().max_agents;
        if agents.len() as u32 >= max {
            bail!("this workspace allows at most {max} agents (settings.max_agents)");
        }
        let base = name.map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&role.name).to_string();
        let mut name = base.clone();
        let mut n = 2;
        while agents.iter().any(|a| a.name.eq_ignore_ascii_case(&name)) {
            name = format!("{base} {n}");
            n += 1;
        }
        let agent = AgentInfo { id: short_id("a"), name, role_id: role.id.clone(), is_lead, created_by: by };
        self.emit(Some(&agent.id), EventKind::AgentCreated { agent: agent.clone() })?;
        self.set_run_state(&agent.id, RunState::Idle);
        Ok(agent)
    }

    // ---------------------------------------------------------- messages

    /// Deliver a message to an agent and wake it.
    pub fn send_message(self: &Arc<Self>, to: &str, from: Sender, text: &str, kind: MessageKind) -> Result<Event> {
        let target = self.store().agent(to)?.with_context(|| format!("no agent {to}"))?;
        let ev = self.emit(Some(&target.id), EventKind::Message { to: target.id.clone(), from: from.clone(), text: text.to_string(), kind })?;
        match &from {
            Sender::User => {
                self.autonomy.store(0, Ordering::SeqCst);
                self.wake(&target.id);
            }
            Sender::Agent { .. } => {
                let n = self.autonomy.fetch_add(1, Ordering::SeqCst) + 1;
                let budget = self.settings().autonomy_budget;
                if budget > 0 && n > budget {
                    self.notice(
                        Some(&target.id),
                        "warn",
                        format!("Autonomy budget reached ({budget} agent-to-agent messages without user input). Send a message to continue."),
                    );
                    self.set_run_state(&target.id, RunState::Paused { reason: "autonomy budget reached".into() });
                } else {
                    self.wake(&target.id);
                }
            }
            Sender::Harness => self.wake(&target.id),
        }
        Ok(ev)
    }

    // -------------------------------------------------------------- runs

    pub fn is_running(&self, agent_id: &str) -> bool {
        self.runs.lock().unwrap().contains_key(agent_id)
    }

    /// Start a run for the agent, or flag the active run to pick up new input.
    pub fn wake(self: &Arc<Self>, agent_id: &str) {
        let mut runs = self.runs.lock().unwrap();
        if let Some(ctl) = runs.get(agent_id) {
            ctl.again.store(true, Ordering::SeqCst);
            return;
        }
        let ctl = RunCtl::new();
        runs.insert(agent_id.to_string(), ctl.clone());
        drop(runs);
        let me = self.clone();
        let id = agent_id.to_string();
        tokio::spawn(async move {
            crate::agent::run(me, id, ctl).await;
        });
    }

    /// Called by a run when it would finish. Returns true if new input
    /// arrived and the run should continue instead.
    pub fn finish_or_continue(&self, agent_id: &str, ctl: &RunCtl) -> bool {
        let mut runs = self.runs.lock().unwrap();
        if ctl.again.swap(false, Ordering::SeqCst) && !ctl.cancel.is_cancelled() {
            return true;
        }
        runs.remove(agent_id);
        false
    }

    /// Remove this run's entry (only if it is still this run's).
    pub fn end_run(&self, agent_id: &str, ctl: &RunCtl) {
        let mut runs = self.runs.lock().unwrap();
        if runs.get(agent_id).is_some_and(|c| Arc::ptr_eq(&c.again, &ctl.again)) {
            runs.remove(agent_id);
        }
    }

    pub fn stop(&self, agent_id: &str) {
        if let Some(ctl) = self.runs.lock().unwrap().get(agent_id) {
            *ctl.reason.lock().unwrap() = StopReason::User;
            ctl.cancel.cancel();
        } else {
            self.set_run_state(agent_id, RunState::Stopped);
        }
        // Reject approvals this agent is waiting on.
        let ids: Vec<String> = self
            .approvals
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, p)| p.agent_id == agent_id)
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            let _ = self.resolve_approval(&id, false, "agent stopped");
        }
    }

    fn cancel_all(&self, reason: StopReason) {
        for ctl in self.runs.lock().unwrap().values() {
            *ctl.reason.lock().unwrap() = reason;
            ctl.cancel.cancel();
        }
    }

    pub async fn pause_all_for_lock(&self) {
        self.cancel_all(StopReason::Lock);
    }

    pub async fn shutdown(&self) {
        self.cancel_all(StopReason::Shutdown);
        for _ in 0..50 {
            if self.runs.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// After a restart or unlock: continue agents that were mid-run or have
    /// unprocessed input.
    pub async fn resume(self: &Arc<Self>) {
        let Ok(agents) = self.agents() else { return };
        for a in agents {
            if self.is_running(&a.id) {
                continue;
            }
            let state = self.store().run_state(&a.id).ok().flatten().unwrap_or(RunState::Idle);
            let resume = match state {
                RunState::Running | RunState::WaitingEngine { .. } | RunState::WaitingApproval | RunState::Locked => true,
                RunState::Idle => crate::context::has_unprocessed_input(self.store(), &a.id).unwrap_or(false),
                RunState::Stopped | RunState::Paused { .. } | RunState::Error { .. } => false,
            };
            if resume {
                tracing::info!("resuming agent {} ({}) in {}", a.name, a.id, self.name());
                self.wake(&a.id);
            }
        }
    }

    // --------------------------------------------------------- approvals

    /// Ask the user to approve a tool call. Resolves to (approved, note).
    pub async fn request_approval(
        &self,
        agent_id: &str,
        call: &ToolCall,
        summary: String,
        cancel: &CancellationToken,
    ) -> Result<(bool, String)> {
        let id = short_id("ap");
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().unwrap().insert(
            id.clone(),
            PendingApproval { agent_id: agent_id.into(), tool: call.name.clone(), summary: summary.clone(), tx },
        );
        self.emit(
            Some(agent_id),
            EventKind::ApprovalRequested {
                approval_id: id.clone(),
                call_id: call.id.clone(),
                tool: call.name.clone(),
                summary,
                arguments: call.arguments.clone(),
            },
        )?;
        self.set_run_state(agent_id, RunState::WaitingApproval);
        let res = tokio::select! {
            r = rx => r.unwrap_or((false, "approval dropped".into())),
            _ = cancel.cancelled() => {
                self.approvals.lock().unwrap().remove(&id);
                return Err(anyhow::anyhow!("cancelled while waiting for approval"));
            }
        };
        self.set_run_state(agent_id, RunState::Running);
        Ok(res)
    }

    pub fn resolve_approval(&self, id: &str, approved: bool, note: &str) -> Result<()> {
        let p = self.approvals.lock().unwrap().remove(id).context("no such pending approval (already resolved?)")?;
        self.emit(
            Some(&p.agent_id),
            EventKind::ApprovalResolved { approval_id: id.into(), approved, note: note.into() },
        )?;
        let _ = p.tx.send((approved, note.to_string()));
        Ok(())
    }

    pub fn pending_approvals(&self) -> Vec<serde_json::Value> {
        self.approvals
            .lock()
            .unwrap()
            .iter()
            .map(|(id, p)| serde_json::json!({"approval_id": id, "agent_id": p.agent_id, "tool": p.tool, "summary": p.summary}))
            .collect()
    }

    // ------------------------------------------------------ environments

    pub async fn env(&self, name: &str) -> Result<Arc<dyn Environment>> {
        let mut envs = self.envs.lock().await;
        if let Some(e) = envs.get(name) {
            return Ok(e.clone());
        }
        let recipe = self
            .dir
            .env_recipes()?
            .into_iter()
            .find(|r| r.name == name)
            .with_context(|| format!("no environment {name:?}"))?;
        let ctx = EnvContext {
            workspace_id: self.id().to_string(),
            files_dir: self.files_dir(),
            env_dir: self.dir.envs_dir().join(name),
            cache_dir: self.rt.home.cache.clone(),
        };
        let e = kk_env::build(recipe, ctx);
        envs.insert(name.to_string(), e.clone());
        Ok(e)
    }

    pub fn env_recipes(&self) -> Result<Vec<EnvRecipe>> {
        self.dir.env_recipes()
    }

    pub async fn define_env(&self, recipe: EnvRecipe) -> Result<()> {
        self.dir.save_env_recipe(&recipe)?;
        self.envs.lock().await.remove(&recipe.name);
        Ok(())
    }

    pub async fn env_statuses(&self) -> Vec<(EnvRecipe, EnvStatus)> {
        let mut out = Vec::new();
        for r in self.env_recipes().unwrap_or_default() {
            let st = match self.env(&r.name).await {
                Ok(e) => e.status().await,
                Err(err) => EnvStatus::Broken { message: format!("{err:#}") },
            };
            out.push((r, st));
        }
        out
    }

    /// Start an environment, streaming progress to the UI.
    pub async fn start_env(self: &Arc<Self>, name: &str) -> Result<()> {
        let env = self.env(name).await?;
        let _ = self.emit(None, EventKind::EnvState { name: name.into(), state: "starting".into(), detail: String::new() });
        let rt = self.rt.clone();
        let wid = self.id().to_string();
        let n = name.to_string();
        let progress = move |line: String| rt.publish(Some(&wid), Live::EnvLog { name: n.clone(), line });
        match env.start(&progress).await {
            Ok(()) => {
                let _ = self.emit(None, EventKind::EnvState { name: name.into(), state: "running".into(), detail: String::new() });
                Ok(())
            }
            Err(e) => {
                let _ = self.emit(None, EventKind::EnvState { name: name.into(), state: "failed".into(), detail: format!("{e:#}") });
                Err(e)
            }
        }
    }

    pub async fn stop_env(&self, name: &str) -> Result<()> {
        self.env(name).await?.stop().await?;
        let _ = self.emit(None, EventKind::EnvState { name: name.into(), state: "stopped".into(), detail: String::new() });
        Ok(())
    }

    /// Prepare for copying the folder elsewhere: stop VMs, flush the database.
    pub async fn prepare_migration(&self) -> Result<()> {
        for r in self.env_recipes()? {
            if let Ok(e) = self.env(&r.name).await {
                e.stop().await?;
            }
        }
        self.store().checkpoint()
    }
}
