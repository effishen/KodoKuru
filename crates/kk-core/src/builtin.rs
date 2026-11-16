//! Built-in roles and templates compiled into the binary. They are read-only:
//! a role or template with the same id in the home folder overrides them, so
//! app updates can improve built-ins without touching user edits.

use crate::{Role, Template};

macro_rules! asset {
    ($kind:literal, $name:literal) => {
        ($name, include_str!(concat!("../../../assets/", $kind, "/", $name, ".toml")))
    };
}

const ROLES: &[(&str, &str)] = &[
    asset!("roles", "assistant"),
    asset!("roles", "architect"),
    asset!("roles", "developer"),
    asset!("roles", "qa"),
    asset!("roles", "investigator"),
    asset!("roles", "reverser"),
    asset!("roles", "writer"),
    asset!("roles", "game-master"),
    asset!("roles", "continuity"),
    asset!("roles", "narrator-voice"),
];

const TEMPLATES: &[(&str, &str)] = &[
    asset!("templates", "general"),
    asset!("templates", "software-dev"),
    asset!("templates", "reverse-engineering"),
    asset!("templates", "game-master"),
];

pub fn roles() -> Vec<Role> {
    ROLES
        .iter()
        .map(|(name, src)| toml::from_str(src).unwrap_or_else(|e| panic!("built-in role {name}: {e}")))
        .collect()
}

pub fn templates() -> Vec<Template> {
    TEMPLATES
        .iter()
        .map(|(name, src)| toml::from_str(src).unwrap_or_else(|e| panic!("built-in template {name}: {e}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse_and_reference_known_roles() {
        let roles = roles();
        for r in &roles {
            r.validate().unwrap();
        }
        for t in templates() {
            let known = |id: &str| roles.iter().any(|r| r.id == id);
            assert!(known(&t.lead_role), "{}: unknown lead role", t.id);
            for r in t.roles.iter().chain(&t.initial_agents) {
                assert!(known(r), "{}: unknown role {r}", t.id);
            }
            for e in &t.environments {
                e.validate().unwrap();
            }
        }
    }
}
