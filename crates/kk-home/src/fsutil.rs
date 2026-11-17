//! Small filesystem helpers: atomic writes and advisory locks.

use anyhow::{Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Write a file atomically: temp file in the same directory, fsync, rename.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    write_atomic_mode(path, data, None)
}

/// Like [`write_atomic`] but with explicit unix permissions (e.g. 0o600).
pub fn write_atomic_mode(path: &Path, data: &[u8], mode: Option<u32>) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    {
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        if let Some(m) = mode {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(m);
        }
        let mut f = opts.open(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("rename into {}", path.display()))?;
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// An exclusive advisory lock held for the lifetime of the value.
#[derive(Debug)]
pub struct FileLock {
    _file: File,
    pub path: PathBuf,
}

/// Try to take an exclusive lock on `path` (created if missing). Returns
/// `Ok(None)` if another process holds it.
pub fn try_lock(path: &Path) -> Result<Option<FileLock>> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("open lock {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(FileLock { _file: file, path: path.to_path_buf() })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e).with_context(|| format!("lock {}", path.display())),
    }
}

pub fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

pub fn write_toml<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = toml::to_string_pretty(value)?;
    write_atomic(path, text.as_bytes())
}
