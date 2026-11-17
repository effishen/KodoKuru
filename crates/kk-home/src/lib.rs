//! The KodoKuru home folder (portable: copy it to migrate), the cache folder
//! (disposable), per-machine settings and the secret vault.
//!
//! ```text
//! kodokuru-home/
//!   kodokuru.toml        global settings + schema version
//!   engines.toml         engine catalog
//!   vault.json           encrypted secrets
//!   roles/               role library (overrides built-ins by id)
//!   templates/           user templates (override built-ins by id)
//!   machines/<id>.toml   per-machine settings, picked automatically
//!   machines/<id>/tls/   per-machine TLS certificate
//!   workspaces/          one folder per workspace
//!   logs/  run/
//! ```

pub mod fsutil;
pub mod vault;

use anyhow::{bail, Context, Result};
use kk_core::{EnginesFile, HomeSettings, MachineSettings, Role, Template, HOME_SCHEMA_VERSION};
use std::path::{Path, PathBuf};

pub use fsutil::{try_lock, write_atomic, FileLock};

/// Where a role or template definition came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Builtin,
    Library,
    /// A library entry that replaces a built-in with the same id.
    Override,
}

#[derive(Debug, Clone)]
pub struct Home {
    pub root: PathBuf,
    pub cache: PathBuf,
    pub settings: HomeSettings,
    pub machine: MachineSettings,
}

const ENGINES_TEMPLATE: &str = r#"# KodoKuru engine catalog. Agents pick engines from this list.
# Edit here or in Settings → Engines. API keys are stored in the vault and
# referenced as "secret:<name>".
default_chat = ""

# [[engine]]
# id = "omlx"
# kind = "chat"
# base_url = "http://192.168.1.50:8000/v1"
# model = "Qwen3-Coder-30B-A3B-Instruct-4bit"
# api_key = "secret:omlx"
# context_window = 65536
# max_output_tokens = 8192
# max_concurrent = 1
# notes = "Local coding model on the Mac. Good tool use."
# cost = "local"
"#;

pub fn default_home_root() -> PathBuf {
    if let Ok(h) = std::env::var("KODOKURU_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local/share"));
    base.join("kodokuru")
}

pub fn default_cache_root() -> PathBuf {
    if let Ok(h) = std::env::var("KODOKURU_CACHE") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".cache"));
    base.join("kodokuru")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Stable id of this machine, derived from /etc/machine-id when available so
/// it survives reinstalling KodoKuru; otherwise generated once into the cache.
pub fn machine_id(cache: &Path) -> String {
    use sha2::{Digest, Sha256};
    for p in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(s) = std::fs::read_to_string(p) {
            let s = s.trim();
            if !s.is_empty() {
                let h = Sha256::digest(format!("kodokuru:{s}").as_bytes());
                return hex::encode(&h[..8]);
            }
        }
    }
    let path = cache.join("machine-id");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    let id = hex::encode(&uuid::Uuid::new_v4().as_bytes()[..8]);
    let _ = write_atomic(&path, id.as_bytes());
    id
}

pub fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "unknown".into())
}

impl Home {
    /// Open a home folder, creating it on first use and upgrading older
    /// layouts (with a backup). Refuses folders written by a newer version.
    pub fn open(root: &Path, cache: &Path) -> Result<Home> {
        std::fs::create_dir_all(root).with_context(|| format!("create home {}", root.display()))?;
        std::fs::create_dir_all(cache).with_context(|| format!("create cache {}", cache.display()))?;
        let root = root.canonicalize()?;
        let cache = cache.canonicalize()?;
        for d in ["roles", "templates", "machines", "workspaces", "logs", "run"] {
            std::fs::create_dir_all(root.join(d))?;
        }
        let settings_path = root.join("kodokuru.toml");
        let settings: HomeSettings = if settings_path.exists() {
            let s: HomeSettings = fsutil::read_toml(&settings_path)?;
            if s.schema_version > HOME_SCHEMA_VERSION {
                bail!(
                    "this home folder was written by a newer KodoKuru (schema {} > {}); update KodoKuru",
                    s.schema_version,
                    HOME_SCHEMA_VERSION
                );
            }
            if s.schema_version < HOME_SCHEMA_VERSION {
                backup_home_config(&root, s.schema_version)?;
                // No layout changes yet between versions; record the upgrade.
                let mut s = s;
                s.schema_version = HOME_SCHEMA_VERSION;
                fsutil::write_toml(&settings_path, &s)?;
                s
            } else {
                s
            }
        } else {
            let s: HomeSettings = toml::from_str(&format!(
                "schema_version = {HOME_SCHEMA_VERSION}\nhome_id = \"{}\"",
                uuid::Uuid::new_v4()
            ))?;
            fsutil::write_toml(&settings_path, &s)?;
            s
        };
        let engines = root.join("engines.toml");
        if !engines.exists() {
            write_atomic(&engines, ENGINES_TEMPLATE.as_bytes())?;
        }

        let mid = machine_id(&cache);
        let mpath = root.join("machines").join(format!("{mid}.toml"));
        let mut machine: MachineSettings = if mpath.exists() {
            fsutil::read_toml(&mpath)?
        } else {
            let cpus = std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(2);
            toml::from_str(&format!(
                "machine_id = \"{mid}\"\nhostname = \"{}\"\nmax_vm_cpus = {cpus}",
                hostname().replace('"', "")
            ))?
        };
        machine.hostname = hostname();
        machine.last_seen = Some(kk_core::now());
        fsutil::write_toml(&mpath, &machine)?;

        Ok(Home { root, cache, settings, machine })
    }

    pub fn settings_path(&self) -> PathBuf {
        self.root.join("kodokuru.toml")
    }
    pub fn engines_path(&self) -> PathBuf {
        self.root.join("engines.toml")
    }
    pub fn vault_path(&self) -> PathBuf {
        self.root.join("vault.json")
    }
    pub fn workspaces_dir(&self) -> PathBuf {
        self.root.join("workspaces")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }
    pub fn run_dir(&self) -> PathBuf {
        self.root.join("run")
    }
    pub fn machine_dir(&self) -> PathBuf {
        self.root.join("machines").join(&self.machine.machine_id)
    }
    pub fn tls_dir(&self) -> PathBuf {
        self.machine_dir().join("tls")
    }

    pub fn port(&self) -> u16 {
        if self.machine.port != 0 {
            self.machine.port
        } else {
            self.settings.port
        }
    }

    /// One running KodoKuru per home folder.
    pub fn lock(&self) -> Result<Option<FileLock>> {
        try_lock(&self.run_dir().join("kodokuru.lock"))
    }

    pub fn save_settings(&self) -> Result<()> {
        fsutil::write_toml(&self.settings_path(), &self.settings)
    }

    // ------------------------------------------------------------ engines

    pub fn load_engines(&self) -> Result<EnginesFile> {
        let f: EnginesFile = fsutil::read_toml(&self.engines_path())?;
        f.validate()?;
        Ok(f)
    }

    pub fn engines_raw(&self) -> Result<String> {
        Ok(std::fs::read_to_string(self.engines_path())?)
    }

    /// Replace engines.toml with validated text (keeps the user's comments).
    pub fn save_engines_raw(&self, text: &str) -> Result<EnginesFile> {
        let f: EnginesFile = toml::from_str(text).context("engines.toml is not valid TOML")?;
        f.validate()?;
        write_atomic(&self.engines_path(), text.as_bytes())?;
        Ok(f)
    }

    // -------------------------------------------------- roles & templates

    pub fn roles(&self) -> Result<Vec<(Role, Source)>> {
        let mut out: Vec<(Role, Source)> =
            kk_core::builtin::roles().into_iter().map(|r| (r, Source::Builtin)).collect();
        for r in load_dir::<Role>(&self.root.join("roles"))? {
            if let Some(slot) = out.iter_mut().find(|(b, _)| b.id == r.id) {
                *slot = (r, Source::Override);
            } else {
                out.push((r, Source::Library));
            }
        }
        Ok(out)
    }

    pub fn role(&self, id: &str) -> Result<Option<Role>> {
        Ok(self.roles()?.into_iter().find(|(r, _)| r.id == id).map(|(r, _)| r))
    }

    pub fn save_role(&self, role: &Role) -> Result<()> {
        role.validate()?;
        fsutil::write_toml(&self.root.join("roles").join(format!("{}.toml", role.id)), role)
    }

    /// Delete a library role. Built-ins cannot be deleted; deleting an
    /// override restores the built-in.
    pub fn delete_role(&self, id: &str) -> Result<bool> {
        anyhow::ensure!(kk_core::valid_ident(id), "invalid role id");
        let p = self.root.join("roles").join(format!("{id}.toml"));
        if p.exists() {
            std::fs::remove_file(p)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn templates(&self) -> Result<Vec<(Template, Source)>> {
        let mut out: Vec<(Template, Source)> =
            kk_core::builtin::templates().into_iter().map(|t| (t, Source::Builtin)).collect();
        for t in load_dir::<Template>(&self.root.join("templates"))? {
            if let Some(slot) = out.iter_mut().find(|(b, _)| b.id == t.id) {
                *slot = (t, Source::Override);
            } else {
                out.push((t, Source::Library));
            }
        }
        Ok(out)
    }
}

/// Load every `*.toml` in a directory, skipping (and logging) broken files so
/// one bad edit does not take the whole library down.
fn load_dir<T: serde::de::DeserializeOwned>(dir: &Path) -> Result<Vec<T>> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return Ok(out) };
    let mut paths: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    paths.sort();
    for p in paths {
        match fsutil::read_toml::<T>(&p) {
            Ok(v) => out.push(v),
            Err(e) => tracing::warn!("skipping {}: {e:#}", p.display()),
        }
    }
    Ok(out)
}

fn backup_home_config(root: &Path, from_version: u32) -> Result<()> {
    let dir = root.join("backups").join(format!(
        "schema-{from_version}-{}",
        kk_core::now().format("%Y%m%d-%H%M%S")
    ));
    std::fs::create_dir_all(&dir)?;
    for f in ["kodokuru.toml", "engines.toml", "vault.json"] {
        let p = root.join(f);
        if p.exists() {
            std::fs::copy(&p, dir.join(f))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_layout_and_is_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let h = Home::open(&d.path().join("home"), &d.path().join("cache")).unwrap();
        assert!(h.engines_path().exists());
        assert!(h.load_engines().unwrap().engines.is_empty());
        let id = h.settings.home_id.clone();
        let h2 = Home::open(&h.root, &h.cache).unwrap();
        assert_eq!(h2.settings.home_id, id);
        assert!(h.root.join("machines").join(format!("{}.toml", h.machine.machine_id)).exists());
    }

    #[test]
    fn library_role_overrides_builtin() {
        let d = tempfile::tempdir().unwrap();
        let h = Home::open(&d.path().join("home"), &d.path().join("cache")).unwrap();
        let mut r = h.role("developer").unwrap().unwrap();
        r.prompt = "custom".into();
        h.save_role(&r).unwrap();
        let roles = h.roles().unwrap();
        let (dev, src) = roles.iter().find(|(r, _)| r.id == "developer").unwrap();
        assert_eq!(dev.prompt, "custom");
        assert_eq!(*src, Source::Override);
        h.delete_role("developer").unwrap();
        assert_ne!(h.role("developer").unwrap().unwrap().prompt, "custom");
    }

    #[test]
    fn single_instance_lock() {
        let d = tempfile::tempdir().unwrap();
        let h = Home::open(&d.path().join("home"), &d.path().join("cache")).unwrap();
        let l = h.lock().unwrap();
        assert!(l.is_some());
        assert!(h.lock().unwrap().is_none());
        drop(l);
        assert!(h.lock().unwrap().is_some());
    }

    #[test]
    fn newer_schema_refused() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("home");
        let h = Home::open(&root, &d.path().join("cache")).unwrap();
        let mut s = h.settings.clone();
        s.schema_version = 99;
        fsutil::write_toml(&h.settings_path(), &s).unwrap();
        assert!(Home::open(&root, &d.path().join("cache")).is_err());
    }
}
