//! Browser sessions. Single user: the vault password is the login.
//! Sessions live in memory; after a restart the first login also unlocks.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, Method, StatusCode};
use rand::RngCore;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::AppState;

pub const COOKIE: &str = "kk_session";
const SESSION_IDLE: Duration = Duration::from_secs(30 * 24 * 3600);

pub struct Sessions {
    map: Mutex<HashMap<String, Instant>>,
    setup_code: Mutex<Option<String>>,
    failures: Mutex<(u32, Option<Instant>)>,
}

impl Sessions {
    pub fn new(setup_code: Option<String>) -> Self {
        Self { map: Default::default(), setup_code: Mutex::new(setup_code), failures: Mutex::new((0, None)) }
    }

    pub fn create(&self) -> String {
        let mut b = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut b);
        let t = hex::encode(b);
        self.map.lock().unwrap().insert(t.clone(), Instant::now());
        t
    }

    pub fn check(&self, headers: &HeaderMap) -> Option<String> {
        let token = cookie(headers, COOKIE)?;
        let mut m = self.map.lock().unwrap();
        match m.get_mut(&token) {
            Some(last) if last.elapsed() < SESSION_IDLE => {
                *last = Instant::now();
                Some(token)
            }
            Some(_) => {
                m.remove(&token);
                None
            }
            None => None,
        }
    }

    pub fn remove(&self, token: &str) {
        self.map.lock().unwrap().remove(token);
    }

    pub fn clear(&self) {
        self.map.lock().unwrap().clear();
    }

    /// Consume the setup code; constant-time compare.
    pub fn take_setup_code(&self, given: &str) -> bool {
        let mut g = self.setup_code.lock().unwrap();
        let Some(code) = g.as_ref() else { return false };
        let a = code.as_bytes();
        let b = given.trim().as_bytes();
        let eq = a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0;
        if eq {
            *g = None;
        }
        eq
    }

    /// Delay to apply before the next password attempt.
    pub fn backoff(&self) -> Duration {
        let f = *self.failures.lock().unwrap();
        match f {
            (n, Some(t)) if n >= 5 && t.elapsed() < Duration::from_secs(300) => Duration::from_secs(10),
            (n, _) if n > 0 => Duration::from_millis(800),
            _ => Duration::ZERO,
        }
    }

    pub fn record(&self, ok: bool) {
        let mut f = self.failures.lock().unwrap();
        if ok {
            *f = (0, None);
        } else {
            f.0 += 1;
            f.1 = Some(Instant::now());
        }
    }
}

pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

pub fn set_cookie(token: &str, secure: bool) -> String {
    format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        SESSION_IDLE.as_secs(),
        if secure { "; Secure" } else { "" }
    )
}

pub fn clear_cookie(secure: bool) -> String {
    format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}", if secure { "; Secure" } else { "" })
}

/// Extractor: a logged-in request. State-changing requests must also carry
/// `X-KodoKuru: 1`, which cross-site forms cannot send (CSRF protection).
pub struct Authed {
    pub token: String,
}

impl FromRequestParts<AppState> for Authed {
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let token = st.sessions.check(&parts.headers).ok_or((StatusCode::UNAUTHORIZED, "log in first"))?;
        if parts.method != Method::GET && parts.headers.get("x-kodokuru").is_none() {
            return Err((StatusCode::FORBIDDEN, "missing X-KodoKuru header"));
        }
        Ok(Authed { token })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_parsing() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "a=1; kk_session=abc; b=2".parse().unwrap());
        assert_eq!(cookie(&h, COOKIE).as_deref(), Some("abc"));
    }

    #[test]
    fn setup_code_single_use() {
        let s = Sessions::new(Some("123456".into()));
        assert!(!s.take_setup_code("000000"));
        assert!(s.take_setup_code("123456"));
        assert!(!s.take_setup_code("123456"));
    }
}
