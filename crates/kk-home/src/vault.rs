//! Password-unlocked secret vault (`vault.json`), modelled on how Proton
//! uses the login password: the password unlocks a key, the key unlocks data.
//!
//! ```text
//! password ──Argon2id(salt)──► KEK ──unwraps──► master key ──decrypts──► secrets
//! recovery key ──Argon2id(salt2)──► KEK2 ──unwraps──┘
//! ```
//!
//! Only wrapped keys and ciphertext are stored. A wrong password simply fails
//! to authenticate the wrapped master key. Changing the password re-wraps the
//! master key only.

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use zeroize::{Zeroize, Zeroizing};

use crate::fsutil::write_atomic_mode;

const AAD_PASSWORD: &[u8] = b"kodokuru-vault-v1:password-wrap";
const AAD_RECOVERY: &[u8] = b"kodokuru-vault-v1:recovery-wrap";
const AAD_SECRETS: &[u8] = b"kodokuru-vault-v1:secrets";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct KdfParams {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl KdfParams {
    /// 64 MiB, 3 passes: a few hundred ms on a desktop CPU.
    pub const DEFAULT: KdfParams = KdfParams { m_cost_kib: 64 * 1024, t_cost: 3, p_cost: 1 };
    /// Cheap parameters for tests only.
    pub const TEST: KdfParams = KdfParams { m_cost_kib: 256, t_cost: 1, p_cost: 1 };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Sealed {
    nonce: String,
    ct: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Wrap {
    salt: String,
    kdf: KdfParams,
    sealed: Sealed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VaultFile {
    version: u32,
    password: Wrap,
    recovery: Wrap,
    secrets: Sealed,
    updated_at: kk_core::Timestamp,
}

/// An unlocked vault. Holds the master key in memory until dropped.
pub struct UnlockedVault {
    path: PathBuf,
    master: Zeroizing<[u8; 32]>,
    secrets: BTreeMap<String, String>,
    file: VaultFile,
}

impl std::fmt::Debug for UnlockedVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnlockedVault")
            .field("path", &self.path)
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Drop for UnlockedVault {
    fn drop(&mut self) {
        for v in self.secrets.values_mut() {
            v.zeroize();
        }
    }
}

fn derive_kek(secret: &[u8], salt: &[u8], p: KdfParams) -> Result<Zeroizing<[u8; 32]>> {
    let params = argon2::Params::new(p.m_cost_kib, p.t_cost, p.p_cost, Some(32))
        .map_err(|e| anyhow!("argon2 params: {e}"))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(secret, salt, out.as_mut())
        .map_err(|e| anyhow!("argon2: {e}"))?;
    Ok(out)
}

fn seal(key: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<Sealed> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut nonce = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad })
        .map_err(|_| anyhow!("encryption failed"))?;
    Ok(Sealed { nonce: B64.encode(nonce), ct: B64.encode(ct) })
}

fn open(key: &[u8; 32], s: &Sealed, aad: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = B64.decode(&s.nonce).ok()?;
    if nonce.len() != 24 {
        return None;
    }
    let ct = B64.decode(&s.ct).ok()?;
    cipher
        .decrypt(XNonce::from_slice(&nonce), Payload { msg: &ct, aad })
        .ok()
        .map(Zeroizing::new)
}

fn make_wrap(secret: &[u8], master: &[u8; 32], aad: &[u8], kdf: KdfParams) -> Result<Wrap> {
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    let kek = derive_kek(secret, &salt, kdf)?;
    Ok(Wrap { salt: B64.encode(salt), kdf, sealed: seal(&kek, master, aad)? })
}

fn unwrap_master(secret: &[u8], w: &Wrap, aad: &[u8]) -> Result<Option<Zeroizing<[u8; 32]>>> {
    let salt = B64.decode(&w.salt).context("corrupt vault salt")?;
    let kek = derive_kek(secret, &salt, w.kdf)?;
    let Some(plain) = open(&kek, &w.sealed, aad) else { return Ok(None) };
    if plain.len() != 32 {
        bail!("corrupt vault: master key has wrong length");
    }
    let mut m = Zeroizing::new([0u8; 32]);
    m.copy_from_slice(&plain);
    Ok(Some(m))
}

/// Human-friendly recovery key: 160 random bits as 8 groups of base32.
fn new_recovery_key() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let chars: Vec<char> = bytes.iter().map(|b| ALPHABET[(*b as usize) % 32] as char).collect();
    chars.chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join("-")
}

fn normalize_recovery(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

pub fn exists(path: &Path) -> bool {
    path.is_file()
}

fn load(path: &Path) -> Result<VaultFile> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let f: VaultFile = serde_json::from_str(&text).context("parse vault")?;
    if f.version != 1 {
        bail!("unsupported vault version {}", f.version);
    }
    Ok(f)
}

#[derive(Debug)]
pub enum UnlockError {
    WrongCredentials,
    Other(anyhow::Error),
}

impl std::fmt::Display for UnlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnlockError::WrongCredentials => write!(f, "wrong password or recovery key"),
            UnlockError::Other(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for UnlockError {}

impl From<anyhow::Error> for UnlockError {
    fn from(e: anyhow::Error) -> Self {
        UnlockError::Other(e)
    }
}

impl UnlockedVault {
    /// Create a new vault. Returns the unlocked vault and the recovery key,
    /// which must be shown to the user exactly once.
    pub fn create(path: &Path, password: &str, kdf: KdfParams) -> Result<(Self, String)> {
        if exists(path) {
            bail!("vault already exists at {}", path.display());
        }
        validate_password(password)?;
        let mut master = Zeroizing::new([0u8; 32]);
        rand::thread_rng().fill_bytes(master.as_mut());
        let recovery = new_recovery_key();
        let file = VaultFile {
            version: 1,
            password: make_wrap(password.as_bytes(), &master, AAD_PASSWORD, kdf)?,
            recovery: make_wrap(normalize_recovery(&recovery).as_bytes(), &master, AAD_RECOVERY, kdf)?,
            secrets: seal(&master, b"{}", AAD_SECRETS)?,
            updated_at: kk_core::now(),
        };
        let v = UnlockedVault { path: path.to_path_buf(), master, secrets: BTreeMap::new(), file };
        v.persist()?;
        Ok((v, recovery))
    }

    pub fn unlock_password(path: &Path, password: &str) -> Result<Self, UnlockError> {
        let file = load(path)?;
        let master = unwrap_master(password.as_bytes(), &file.password, AAD_PASSWORD)?
            .ok_or(UnlockError::WrongCredentials)?;
        Ok(Self::finish(path, master, file)?)
    }

    pub fn unlock_recovery(path: &Path, recovery_key: &str) -> Result<Self, UnlockError> {
        let file = load(path)?;
        let norm = normalize_recovery(recovery_key);
        let master = unwrap_master(norm.as_bytes(), &file.recovery, AAD_RECOVERY)?
            .ok_or(UnlockError::WrongCredentials)?;
        Ok(Self::finish(path, master, file)?)
    }

    fn finish(path: &Path, master: Zeroizing<[u8; 32]>, file: VaultFile) -> Result<Self> {
        let plain = open(&master, &file.secrets, AAD_SECRETS).context("vault secrets do not decrypt (corrupt?)")?;
        let secrets: BTreeMap<String, String> = serde_json::from_slice(&plain).context("parse secrets")?;
        Ok(UnlockedVault { path: path.to_path_buf(), master, secrets, file })
    }

    /// Check a password against the stored wrap (used for web logins while
    /// the vault is already unlocked).
    pub fn verify_password(&self, password: &str) -> Result<bool> {
        Ok(unwrap_master(password.as_bytes(), &self.file.password, AAD_PASSWORD)?
            .map(|m| m.as_ref() == self.master.as_ref())
            .unwrap_or(false))
    }

    pub fn change_password(&mut self, new_password: &str) -> Result<()> {
        validate_password(new_password)?;
        let kdf = self.file.password.kdf;
        self.file.password = make_wrap(new_password.as_bytes(), &self.master, AAD_PASSWORD, kdf)?;
        self.persist()
    }

    /// Replace the recovery key; returns the new one.
    pub fn rotate_recovery(&mut self) -> Result<String> {
        let key = new_recovery_key();
        let kdf = self.file.recovery.kdf;
        self.file.recovery = make_wrap(normalize_recovery(&key).as_bytes(), &self.master, AAD_RECOVERY, kdf)?;
        self.persist()?;
        Ok(key)
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.secrets.get(name).map(String::as_str)
    }

    pub fn names(&self) -> Vec<String> {
        self.secrets.keys().cloned().collect()
    }

    pub fn set(&mut self, name: &str, value: &str) -> Result<()> {
        anyhow::ensure!(kk_core::valid_ident(name), "invalid secret name {name:?}");
        self.secrets.insert(name.to_string(), value.to_string());
        self.persist()
    }

    pub fn remove(&mut self, name: &str) -> Result<bool> {
        let had = self.secrets.remove(name).is_some();
        if had {
            self.persist()?;
        }
        Ok(had)
    }

    fn persist(&self) -> Result<()> {
        let mut file = self.file.clone();
        let plain = Zeroizing::new(serde_json::to_vec(&self.secrets)?);
        file.secrets = seal(&self.master, &plain, AAD_SECRETS)?;
        file.updated_at = kk_core::now();
        let text = serde_json::to_vec_pretty(&file)?;
        write_atomic_mode(&self.path, &text, Some(0o600))
    }
}

pub fn validate_password(pw: &str) -> Result<()> {
    if pw.chars().count() < 8 {
        bail!("password must be at least 8 characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_unlock_change_recover() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("vault.json");
        let (mut v, recovery) = UnlockedVault::create(&p, "correct horse", KdfParams::TEST).unwrap();
        v.set("omlx", "sk-local-123").unwrap();
        drop(v);

        assert!(matches!(
            UnlockedVault::unlock_password(&p, "wrong password"),
            Err(UnlockError::WrongCredentials)
        ));
        let mut v = UnlockedVault::unlock_password(&p, "correct horse").unwrap();
        assert_eq!(v.get("omlx"), Some("sk-local-123"));
        assert!(v.verify_password("correct horse").unwrap());
        assert!(!v.verify_password("nope nope").unwrap());

        v.change_password("new password!").unwrap();
        drop(v);
        assert!(UnlockedVault::unlock_password(&p, "correct horse").is_err());
        let v = UnlockedVault::unlock_password(&p, "new password!").unwrap();
        assert_eq!(v.get("omlx"), Some("sk-local-123"));
        drop(v);

        // Recovery key works, with sloppy formatting.
        let sloppy = recovery.to_lowercase().replace('-', " ");
        let v = UnlockedVault::unlock_recovery(&p, &sloppy).unwrap();
        assert_eq!(v.get("omlx"), Some("sk-local-123"));
    }

    #[test]
    fn file_contains_no_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("vault.json");
        let (mut v, _) = UnlockedVault::create(&p, "password123", KdfParams::TEST).unwrap();
        v.set("key", "super-secret-value").unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(!raw.contains("super-secret-value"));
        assert!(!raw.contains("password123"));
    }

    #[test]
    fn short_password_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(UnlockedVault::create(&dir.path().join("v.json"), "short", KdfParams::TEST).is_err());
    }
}
