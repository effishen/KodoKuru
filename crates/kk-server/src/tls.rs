//! Self-signed TLS certificate per machine, kept in
//! `home/machines/<id>/tls/`. Regenerated when the machine's addresses change.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

pub struct CertFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub fingerprint: String,
}

/// The machine's primary LAN address (the one used for outbound traffic).
pub fn primary_ip() -> Option<IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?; // TEST-NET-1: nothing is sent
    s.local_addr().ok().map(|a| a.ip())
}

fn names() -> Vec<String> {
    let mut v = vec!["localhost".to_string(), "127.0.0.1".to_string(), "::1".to_string()];
    let h = kk_home::hostname();
    if !h.is_empty() && h != "unknown" {
        v.push(h.clone());
        v.push(format!("{h}.local"));
    }
    if let Some(ip) = primary_ip() {
        v.push(ip.to_string());
    }
    v.sort();
    v.dedup();
    v
}

pub fn ensure(dir: &Path) -> Result<CertFiles> {
    std::fs::create_dir_all(dir)?;
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    let names_file = dir.join("names.txt");
    let want = names().join("\n");
    let current = std::fs::read_to_string(&names_file).unwrap_or_default();
    if !cert.exists() || !key.exists() || current != want {
        let mut params = rcgen::CertificateParams::new(want.lines().map(str::to_string).collect::<Vec<_>>())
            .context("certificate parameters")?;
        params.distinguished_name.push(rcgen::DnType::CommonName, "KodoKuru");
        params.not_before = rcgen::date_time_ymd(2024, 1, 1);
        params.not_after = rcgen::date_time_ymd(2036, 1, 1);
        let kp = rcgen::KeyPair::generate().context("generate key")?;
        let c = params.self_signed(&kp).context("self-sign")?;
        kk_home::fsutil::write_atomic(&cert, c.pem().as_bytes())?;
        kk_home::fsutil::write_atomic_mode(&key, kp.serialize_pem().as_bytes(), Some(0o600))?;
        kk_home::fsutil::write_atomic(&names_file, want.as_bytes())?;
        tracing::info!("generated a new TLS certificate in {}", dir.display());
    }
    let pem = std::fs::read_to_string(&cert)?;
    let der = pem_to_der(&pem).context("read certificate")?;
    let fp = Sha256::digest(&der);
    let fingerprint = fp.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":");
    Ok(CertFiles { cert, key, fingerprint })
}

fn pem_to_der(pem: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}
