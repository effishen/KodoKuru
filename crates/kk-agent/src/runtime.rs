//! Process-wide state: the home folder, engine catalog, vault, inference
//! scheduling, engine health, and the open workspaces.

use anyhow::{bail, Context, Result};
use kk_core::*;
use kk_home::vault::UnlockedVault;
use kk_home::Home;
use kk_llm::Client;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock, Semaphore};

use crate::workspace::Workspace;
use crate::{Live, LiveMsg};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EngineHealth {
    pub id: String,
    pub kind: String,
    /// `online`, `offline`, `unknown`, `locked` (key in vault, vault locked)
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub models: Vec<String>,
    pub busy: u32,
    pub capacity: u32,
    pub checked_at: Option<Timestamp>,
}

pub struct Runtime {
    pub home: Home,
    pub client: Client,
    engines: RwLock<EnginesFile>,
    vault: RwLock<Option<UnlockedVault>>,
    slots: std::sync::Mutex<HashMap<String, (u32, Arc<Semaphore>)>>,
    health: RwLock<Vec<EngineHealth>>,
    workspaces: RwLock<HashMap<String, Arc<Workspace>>>,
    pub live: broadcast::Sender<LiveMsg>,
}

impl Runtime {
    pub fn new(home: Home) -> Result<Arc<Runtime>> {
        let engines = home.load_engines().unwrap_or_else(|e| {
            tracing::error!("engines.toml: {e:#}");
            EnginesFile::default()
        });
        let (live, _) = broadcast::channel(4096);
        Ok(Arc::new(Runtime {
            home,
            client: Client::new(),
            engines: RwLock::new(engines),
            vault: RwLock::new(None),
            slots: Default::default(),
            health: RwLock::new(Vec::new()),
            workspaces: RwLock::new(HashMap::new()),
            live,
        }))
    }

    pub fn publish(&self, workspace_id: Option<&str>, body: Live) {
        let _ = self.live.send(LiveMsg { workspace_id: workspace_id.map(str::to_string), body });
    }

    // ------------------------------------------------------------- vault

    pub fn vault_exists(&self) -> bool {
        kk_home::vault::exists(&self.home.vault_path())
    }

    pub async fn is_locked(&self) -> bool {
        self.vault.read().await.is_none()
    }

    /// Install an unlocked vault and resume every workspace's agents.
    pub async fn unlock(self: &Arc<Self>, v: UnlockedVault) {
        *self.vault.write().await = Some(v);
        self.publish(None, Live::Lock { locked: false });
        let ws: Vec<_> = self.workspaces.read().await.values().cloned().collect();
        for w in ws {
            w.resume().await;
        }
        let me = self.clone();
        tokio::spawn(async move { me.check_health().await });
    }

    /// Forget the master key and pause runs (they resume on unlock).
    pub async fn lock(&self) {
        let ws: Vec<_> = self.workspaces.read().await.values().cloned().collect();
        for w in ws {
            w.pause_all_for_lock().await;
        }
        *self.vault.write().await = None;
        self.publish(None, Live::Lock { locked: true });
    }

    pub async fn with_vault<T>(&self, f: impl FnOnce(&mut UnlockedVault) -> Result<T>) -> Result<T> {
        let mut g = self.vault.write().await;
        let v = g.as_mut().context("KodoKuru is locked")?;
        f(v)
    }

    pub async fn verify_password(&self, pw: &str) -> Result<bool> {
        let g = self.vault.read().await;
        match g.as_ref() {
            Some(v) => v.verify_password(pw),
            None => Ok(false),
        }
    }

    /// Resolve `secret:<name>`, `env:<VAR>` or a literal key.
    pub async fn resolve_key(&self, spec: &str) -> Result<Option<String>> {
        if spec.is_empty() {
            return Ok(None);
        }
        if let Some(name) = spec.strip_prefix("secret:") {
            let g = self.vault.read().await;
            let v = g.as_ref().context("KodoKuru is locked; log in to unlock secrets")?;
            return Ok(Some(v.get(name).with_context(|| format!("secret {name:?} is not in the vault"))?.to_string()));
        }
        if let Some(var) = spec.strip_prefix("env:") {
            return Ok(std::env::var(var).ok());
        }
        Ok(Some(spec.to_string()))
    }

    // ----------------------------------------------------------- engines

    pub async fn engines(&self) -> EnginesFile {
        self.engines.read().await.clone()
    }

    pub async fn engine(&self, id: &str) -> Option<EngineConfig> {
        self.engines.read().await.get(id).cloned()
    }

    pub async fn set_engines(self: &Arc<Self>, text: &str) -> Result<()> {
        let f = self.home.save_engines_raw(text)?;
        *self.engines.write().await = f;
        let me = self.clone();
        tokio::spawn(async move { me.check_health().await });
        Ok(())
    }

    /// The chat engine for a role in a workspace.
    pub async fn chat_engine_for(&self, role: &Role, settings: &WorkspaceSettings) -> Result<EngineConfig> {
        let engines = self.engines.read().await;
        let candidates = [
            role.engine.clone().unwrap_or_default(),
            settings.default_engine.clone(),
            engines.default_chat.clone(),
        ];
        // The most specific configured choice wins; a bad one is an error, not a fallback.
        if let Some(id) = candidates.iter().find(|s| !s.is_empty()) {
            return match engines.get(id) {
                Some(e) if e.kind == EngineKind::Chat => {
                    if !settings.engine_allowed(&e.id) {
                        bail!("engine {:?} is not allowed in this workspace", e.id);
                    }
                    Ok(e.clone())
                }
                Some(_) => bail!("engine {id:?} is not a chat engine"),
                None => bail!("engine {id:?} is not defined in engines.toml"),
            };
        }
        engines
            .engines
            .iter()
            .find(|e| e.kind == EngineKind::Chat && settings.engine_allowed(&e.id))
            .cloned()
            .context("no chat engine configured — add one in Settings → Engines")
    }

    /// Inference slot for an engine (limits concurrent requests).
    pub fn slot(&self, e: &EngineConfig) -> Arc<Semaphore> {
        let mut m = self.slots.lock().unwrap();
        match m.get(&e.id) {
            Some((n, s)) if *n == e.max_concurrent => s.clone(),
            _ => {
                let s = Arc::new(Semaphore::new(e.max_concurrent as usize));
                m.insert(e.id.clone(), (e.max_concurrent, s.clone()));
                s
            }
        }
    }

    pub async fn health(&self) -> Vec<EngineHealth> {
        let mut h = self.health.read().await.clone();
        let slots = self.slots.lock().unwrap();
        for e in &mut h {
            if let Some((n, s)) = slots.get(&e.id) {
                e.capacity = *n;
                e.busy = n.saturating_sub(s.available_permits() as u32);
            }
        }
        h
    }

    pub async fn check_health(&self) {
        let engines = self.engines().await;
        let mut out = Vec::new();
        for e in &engines.engines {
            let mut h = EngineHealth {
                id: e.id.clone(),
                kind: e.kind.as_str().into(),
                status: "unknown".into(),
                detail: None,
                models: vec![],
                busy: 0,
                capacity: e.max_concurrent,
                checked_at: Some(now()),
            };
            match self.resolve_key(&e.api_key).await {
                Err(err) => {
                    h.status = "locked".into();
                    h.detail = Some(format!("{err:#}"));
                }
                Ok(key) => match self.client.health(e, key.as_deref()).await {
                    Ok(models) => {
                        h.status = "online".into();
                        if !e.model.is_empty() && !models.is_empty() && !models.iter().any(|m| m == &e.model) {
                            h.detail = Some(format!("model {:?} not listed by the server", e.model));
                        }
                        h.models = models;
                    }
                    // Some servers don't implement /models; any HTTP answer means it's up.
                    Err(kk_llm::LlmError::Rejected(code, _)) if code == 404 || code == 405 => {
                        h.status = "online".into();
                        h.detail = Some("server has no /models endpoint".into());
                    }
                    Err(err) => {
                        h.status = "offline".into();
                        h.detail = Some(kk_llm::truncate(&err.to_string(), 200));
                    }
                },
            }
            out.push(h);
        }
        let changed = *self.health.read().await != out;
        *self.health.write().await = out;
        if changed {
            let h = self.health().await;
            self.publish(None, Live::EngineHealth { health: h });
        }
    }

    pub fn spawn_health_loop(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                me.check_health().await;
                tokio::time::sleep(Duration::from_secs(20)).await;
            }
        });
    }

    // -------------------------------------------------------- workspaces

    /// Open every workspace folder in the home.
    pub async fn open_all(self: &Arc<Self>) -> Result<()> {
        let dir = self.home.workspaces_dir();
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if !kk_store::WorkspaceDir::is_workspace(&p) {
                continue;
            }
            match kk_store::WorkspaceDir::open(&p) {
                Ok(d) => {
                    let w = Workspace::new(self.clone(), d);
                    self.workspaces.write().await.insert(w.id().to_string(), w);
                }
                Err(e) => tracing::error!("cannot open workspace {}: {e:#}", p.display()),
            }
        }
        Ok(())
    }

    pub async fn workspaces(&self) -> Vec<Arc<Workspace>> {
        let mut v: Vec<_> = self.workspaces.read().await.values().cloned().collect();
        v.sort_by_key(|w| std::cmp::Reverse(w.created_at()));
        v
    }

    pub async fn workspace(&self, id: &str) -> Option<Arc<Workspace>> {
        self.workspaces.read().await.get(id).cloned()
    }

    pub async fn create_workspace(self: &Arc<Self>, name: &str, template_id: &str) -> Result<Arc<Workspace>> {
        anyhow::ensure!(!name.trim().is_empty(), "workspace needs a name");
        let (template, _) = self
            .home
            .templates()?
            .into_iter()
            .find(|(t, _)| t.id == template_id)
            .with_context(|| format!("unknown template {template_id:?}"))?;
        let roles: Vec<Role> = self.home.roles()?.into_iter().map(|(r, _)| r).collect();
        let dir = kk_store::WorkspaceDir::create(&self.home.workspaces_dir(), name, &template, &roles)?;
        let w = Workspace::new(self.clone(), dir);
        w.initialize(&template).await?;
        self.workspaces.write().await.insert(w.id().to_string(), w.clone());
        self.publish(None, Live::WorkspacesChanged);
        Ok(w)
    }

    /// Stop all runs without marking them stopped, so they resume next start.
    pub async fn shutdown(&self) {
        let ws: Vec<_> = self.workspaces.read().await.values().cloned().collect();
        for w in ws {
            w.shutdown().await;
        }
    }
}
