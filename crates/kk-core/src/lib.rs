//! Shared domain types for KodoKuru: configuration files, the event model,
//! tasks, checklists and the built-in templates/roles shipped with the app.

pub mod builtin;
pub mod config;
pub mod event;
pub mod task;

pub use config::*;
pub use event::*;
pub use task::*;

/// Current on-disk schema version of a home folder and of a workspace.
/// Bump when the layout or database schema changes and add a migration.
pub const HOME_SCHEMA_VERSION: u32 = 1;
pub const WORKSPACE_SCHEMA_VERSION: u32 = 1;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub type Timestamp = chrono::DateTime<chrono::Utc>;

pub fn now() -> Timestamp {
    chrono::Utc::now()
}

/// Short random id with a readable prefix, e.g. `a-3f9k2q`.
pub fn short_id(prefix: &str) -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    let tail: String = (0..6)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect();
    format!("{prefix}-{tail}")
}

/// Turn a display name into a safe identifier / folder name.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "untitled".into()
    } else {
        out.chars().take(48).collect()
    }
}

/// Validate an identifier used for roles, engines and environments.
pub fn valid_ident(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !id.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("My Cool Project!"), "my-cool-project");
        assert_eq!(slugify("  "), "untitled");
        assert_eq!(slugify("a//b"), "a-b");
    }

    #[test]
    fn idents() {
        assert!(valid_ident("qwen3-coder"));
        assert!(!valid_ident("../etc"));
        assert!(!valid_ident(""));
        assert!(!valid_ident(".hidden"));
    }
}
