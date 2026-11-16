//! A workspace folder on disk. Self-contained and portable: copy the folder
//! (with KodoKuru stopped) and open it on another machine.
//!
//! ```text
//! <workspace>/
//!   workspace.toml     id, name, template, schema version, settings
//!   kodokuru.db        event log + projections
//!   files/             the project's files (mounted into VMs at /workspace)
//!   roles/             roles used by this workspace (copied in)
//!   envs/<name>/       env.toml recipe (+ VM disk when kept)
//!   attachments/
//!   .lock              held while open, so two machines can't share it
//! ```

use anyhow::{bail, Context, Result};
use kk_core::*;
use kk_home::{fsutil, FileLock};
use std::path::{Component, Path, PathBuf};

use crate::db::Store;

pub struct WorkspaceDir {
    pub root: PathBuf,
    pub manifest: WorkspaceManifest,
    pub store: Store,
    _lock: FileLock,
}

impl std::fmt::Debug for WorkspaceDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceDir").field("root", &self.root).field("id", &self.manifest.id).finish()
    }
}

impl WorkspaceDir {
    /// Create a new workspace folder from a template. Roles named by the
    /// template are copied in so the workspace works without the library.
    pub fn create(parent: &Path, name: &str, template: &Template, roles: &[Role]) -> Result<WorkspaceDir> {
        let mut slug = slugify(name);
        let mut root = parent.join(&slug);
        let mut n = 2;
        while root.exists() {
            slug = format!("{}-{n}", slugify(name));
            root = parent.join(&slug);
            n += 1;
        }
        for d in ["files", "roles", "envs", "attachments"] {
            std::fs::create_dir_all(root.join(d))?;
        }
        let manifest = WorkspaceManifest {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.trim().to_string(),
            template: template.id.clone(),
            schema_version: WORKSPACE_SCHEMA_VERSION,
            created_at: now(),
            workspace_prompt: template.workspace_prompt.clone(),
            settings: template.settings.clone(),
        };
        fsutil::write_toml(&root.join("workspace.toml"), &manifest)?;
        for id in template.roles.iter().chain(std::iter::once(&template.lead_role)) {
            if let Some(r) = roles.iter().find(|r| &r.id == id) {
                fsutil::write_toml(&root.join("roles").join(format!("{}.toml", r.id)), r)?;
            }
        }
        for env in &template.environments {
            std::fs::create_dir_all(root.join("envs").join(&env.name))?;
            fsutil::write_toml(&root.join("envs").join(&env.name).join("env.toml"), env)?;
        }
        Self::open(&root)
    }

    pub fn open(root: &Path) -> Result<WorkspaceDir> {
        let root = root.canonicalize().with_context(|| format!("workspace {}", root.display()))?;
        let lock = kk_home::try_lock(&root.join(".lock"))?.with_context(|| {
            format!("workspace {} is already open (by this or another KodoKuru)", root.display())
        })?;
        let manifest: WorkspaceManifest = fsutil::read_toml(&root.join("workspace.toml"))?;
        if manifest.schema_version > WORKSPACE_SCHEMA_VERSION {
            bail!("workspace {} was written by a newer KodoKuru", manifest.name);
        }
        for d in ["files", "roles", "envs", "attachments"] {
            std::fs::create_dir_all(root.join(d))?;
        }
        let store = Store::open(&root.join("kodokuru.db"))?;
        Ok(WorkspaceDir { root, manifest, store, _lock: lock })
    }

    pub fn is_workspace(path: &Path) -> bool {
        path.join("workspace.toml").is_file()
    }

    pub fn files_dir(&self) -> PathBuf {
        self.root.join("files")
    }
    pub fn envs_dir(&self) -> PathBuf {
        self.root.join("envs")
    }

    pub fn save_manifest(&mut self) -> Result<()> {
        fsutil::write_toml(&self.root.join("workspace.toml"), &self.manifest)
    }

    pub fn roles(&self) -> Result<Vec<Role>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(self.root.join("roles"))? {
            let p = e?.path();
            if p.extension().is_some_and(|x| x == "toml") {
                match fsutil::read_toml::<Role>(&p) {
                    Ok(r) => out.push(r),
                    Err(e) => tracing::warn!("skipping role {}: {e:#}", p.display()),
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    pub fn role(&self, id: &str) -> Result<Option<Role>> {
        Ok(self.roles()?.into_iter().find(|r| r.id == id))
    }

    pub fn save_role(&self, role: &Role) -> Result<()> {
        role.validate()?;
        fsutil::write_toml(&self.root.join("roles").join(format!("{}.toml", role.id)), role)
    }

    pub fn env_recipes(&self) -> Result<Vec<EnvRecipe>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(self.envs_dir())? {
            let p = e?.path().join("env.toml");
            if p.is_file() {
                match fsutil::read_toml::<EnvRecipe>(&p) {
                    Ok(r) => out.push(r),
                    Err(e) => tracing::warn!("skipping env {}: {e:#}", p.display()),
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn save_env_recipe(&self, recipe: &EnvRecipe) -> Result<()> {
        recipe.validate()?;
        let dir = self.envs_dir().join(&recipe.name);
        std::fs::create_dir_all(&dir)?;
        fsutil::write_toml(&dir.join("env.toml"), recipe)
    }
}

/// Resolve a user/model-supplied path inside `root`, refusing anything that
/// escapes it (via `..`, absolute paths or symlinks). `/workspace/...` (the
/// path inside VMs) is accepted and mapped to the same file.
pub fn resolve_in(root: &Path, rel: &str) -> Result<PathBuf> {
    let rel = rel.trim();
    let rel = rel
        .strip_prefix("/workspace/")
        .or_else(|| if rel == "/workspace" { Some("") } else { None })
        .unwrap_or(rel);
    let rel = rel.trim_start_matches("./");
    let p = Path::new(rel);
    if p.is_absolute() {
        bail!("absolute paths are not allowed; use a path relative to the workspace files (got {rel:?})");
    }
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    bail!("path {rel:?} escapes the workspace");
                }
            }
            _ => bail!("invalid path {rel:?}"),
        }
    }
    let root = root.canonicalize()?;
    let full = root.join(&out);
    // Check the deepest existing ancestor resolves inside root (symlinks).
    let mut probe = full.clone();
    loop {
        if probe.exists() {
            let real = probe.canonicalize()?;
            if !real.starts_with(&root) {
                bail!("path {rel:?} resolves outside the workspace");
            }
            break;
        }
        if !probe.pop() {
            break;
        }
    }
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join("a")).unwrap();
        assert!(resolve_in(root, "a/b.txt").unwrap().ends_with("a/b.txt"));
        assert!(resolve_in(root, "/workspace/a/b.txt").unwrap().ends_with("a/b.txt"));
        assert!(resolve_in(root, "a/../b").is_ok());
        assert!(resolve_in(root, "../etc/passwd").is_err());
        assert!(resolve_in(root, "/etc/passwd").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", root.join("evil")).unwrap();
            assert!(resolve_in(root, "evil/passwd").is_err());
        }
    }

    #[test]
    fn create_from_template() {
        let d = tempfile::tempdir().unwrap();
        let t = kk_core::builtin::templates().into_iter().find(|t| t.id == "software-dev").unwrap();
        let roles = kk_core::builtin::roles();
        let ws = WorkspaceDir::create(d.path(), "My App", &t, &roles).unwrap();
        assert!(ws.root.ends_with("my-app"));
        assert!(ws.role("architect").unwrap().is_some());
        assert_eq!(ws.env_recipes().unwrap()[0].name, "dev");
        // Second open while held fails.
        assert!(WorkspaceDir::open(&ws.root).is_err());
        let ws2 = WorkspaceDir::create(d.path(), "My App", &t, &roles).unwrap();
        assert!(ws2.root.ends_with("my-app-2"));
    }
}
