//! Host prerequisite checks (`kodokuru doctor`) and the guided installer
//! used at startup: KVM access, QEMU, the SFTP server Lima's shared folders
//! need, and Lima itself (downloaded into the cache when missing).

use serde::Serialize;
use std::path::Path;

use crate::{is_executable, which};

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    /// apt package that fixes it, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apt: Option<String>,
    /// Human instruction when it can't be fixed automatically.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Only needed for VM environments.
    pub vm_only: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    pub fn vm_ready(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }
    pub fn missing_apt(&self) -> Vec<String> {
        let mut v: Vec<String> = self.checks.iter().filter(|c| !c.ok).filter_map(|c| c.apt.clone()).collect();
        v.sort();
        v.dedup();
        v
    }
    pub fn render(&self) -> String {
        self.checks
            .iter()
            .map(|c| {
                let mark = if c.ok { "ok " } else { "!! " };
                let mut s = format!("{mark} {:<22} {}", c.name, c.detail);
                if !c.ok {
                    if let Some(h) = &c.hint {
                        s.push_str(&format!("\n      → {h}"));
                    }
                }
                s
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn access_rw(p: &Path) -> bool {
    use std::ffi::CString;
    let Ok(c) = CString::new(p.to_string_lossy().as_bytes()) else { return false };
    unsafe { libc::access(c.as_ptr(), libc::R_OK | libc::W_OK) == 0 }
}

pub fn qemu_binary() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "qemu-system-aarch64",
        _ => "qemu-system-x86_64",
    }
}

fn qemu_package() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "qemu-system-arm",
        _ => "qemu-system-x86",
    }
}

pub fn check(cache: &Path) -> Report {
    let mut checks = Vec::new();
    let is_root = unsafe { libc::geteuid() } == 0;
    checks.push(Check {
        name: "user".into(),
        ok: !is_root,
        detail: if is_root { "running as root".into() } else { "normal user".into() },
        apt: None,
        hint: Some("Lima refuses to run as root; start KodoKuru as your normal user".into()),
        vm_only: true,
    });

    let kvm = Path::new("/dev/kvm");
    let (ok, detail, hint) = if !kvm.exists() {
        (
            false,
            "/dev/kvm missing".to_string(),
            "enable virtualization; on Proxmox enable nested virtualization on the host and set this VM's CPU type to `host`".to_string(),
        )
    } else if !access_rw(kvm) {
        (
            false,
            "/dev/kvm not accessible".to_string(),
            "add yourself to the kvm group: `sudo usermod -aG kvm $USER`, then log in again".to_string(),
        )
    } else {
        (true, "/dev/kvm accessible".to_string(), String::new())
    };
    checks.push(Check { name: "kvm".into(), ok, detail, apt: None, hint: Some(hint), vm_only: true });

    let q = which(qemu_binary());
    checks.push(Check {
        name: "qemu".into(),
        ok: q.is_some(),
        detail: q.map(|p| p.display().to_string()).unwrap_or_else(|| format!("{} not found", qemu_binary())),
        apt: Some(qemu_package().into()),
        hint: Some(format!("sudo apt-get install {} qemu-utils", qemu_package())),
        vm_only: true,
    });
    let qi = which("qemu-img");
    checks.push(Check {
        name: "qemu-img".into(),
        ok: qi.is_some(),
        detail: qi.map(|p| p.display().to_string()).unwrap_or_else(|| "not found".into()),
        apt: Some("qemu-utils".into()),
        hint: Some("sudo apt-get install qemu-utils".into()),
        vm_only: true,
    });

    let sftp = ["/usr/lib/openssh/sftp-server", "/usr/lib/ssh/sftp-server", "/usr/libexec/sftp-server", "/usr/libexec/openssh/sftp-server"]
        .iter()
        .find(|p| is_executable(Path::new(p)));
    checks.push(Check {
        name: "sftp-server".into(),
        ok: sftp.is_some(),
        detail: sftp.map(|p| p.to_string()).unwrap_or_else(|| "not found (needed for shared folders)".into()),
        apt: Some("openssh-sftp-server".into()),
        hint: Some("sudo apt-get install openssh-sftp-server".into()),
        vm_only: true,
    });

    let lima = crate::lima::limactl_path(cache);
    checks.push(Check {
        name: "lima".into(),
        ok: lima.is_some(),
        detail: lima
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| format!("not installed (KodoKuru can install Lima {})", crate::lima::LIMA_VERSION)),
        apt: None,
        hint: Some("run `kodokuru doctor --fix`".into()),
        vm_only: true,
    });
    Report { checks }
}
