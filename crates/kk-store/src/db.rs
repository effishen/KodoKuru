//! SQLite event store for one workspace (`kodokuru.db`). Events are the
//! source of truth; small projection tables are kept in the same
//! transaction for fast queries.

use anyhow::{Context, Result};
use kk_core::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

pub struct Store {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL,
    agent_id TEXT,
    visibility TEXT NOT NULL,
    type TEXT NOT NULL,
    data TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_agent ON events(agent_id, seq);
CREATE TABLE IF NOT EXISTS agents (id TEXT PRIMARY KEY, created_seq INTEGER NOT NULL, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS tasks (id TEXT PRIMARY KEY, num INTEGER NOT NULL, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS checklists (agent_id TEXT PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS world_state (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS run_states (agent_id TEXT PRIMARY KEY, data TEXT NOT NULL);
"#;

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, String, Option<String>, String, String)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
}

fn decode(raw: (i64, String, Option<String>, String, String)) -> Result<Event> {
    let (seq, ts, agent_id, vis, data) = raw;
    let kind: EventKind = serde_json::from_str(&data).with_context(|| format!("decode event {seq}"))?;
    Ok(Event {
        seq,
        ts: chrono::DateTime::parse_from_rfc3339(&ts)?.with_timezone(&chrono::Utc),
        agent_id,
        visibility: if vis == "internal" { Visibility::Internal } else { Visibility::Public },
        kind,
    })
}

fn type_name(kind: &EventKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
        .unwrap_or_default()
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        let version: Option<String> =
            conn.query_row("SELECT value FROM meta WHERE key='schema'", [], |r| r.get(0)).optional()?;
        match version.as_deref().map(str::parse::<u32>) {
            None => {
                conn.execute(
                    "INSERT INTO meta(key,value) VALUES('schema',?1)",
                    params![WORKSPACE_SCHEMA_VERSION.to_string()],
                )?;
            }
            Some(Ok(v)) if v > WORKSPACE_SCHEMA_VERSION => {
                anyhow::bail!("workspace database schema {v} is newer than this KodoKuru supports")
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => anyhow::bail!("bad schema version: {e}"),
        }
        Ok(Store { conn: Mutex::new(conn) })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Append an event and update projections atomically.
    pub fn append(&self, ev: NewEvent) -> Result<Event> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let ts = now();
        let data = serde_json::to_string(&ev.kind)?;
        let vis = match ev.visibility {
            Visibility::Public => "public",
            Visibility::Internal => "internal",
        };
        tx.execute(
            "INSERT INTO events(ts, agent_id, visibility, type, data) VALUES (?1,?2,?3,?4,?5)",
            params![ts.to_rfc3339(), ev.agent_id, vis, type_name(&ev.kind), data],
        )?;
        let seq = tx.last_insert_rowid();
        match &ev.kind {
            EventKind::AgentCreated { agent } => {
                tx.execute(
                    "INSERT OR REPLACE INTO agents(id, created_seq, data) VALUES (?1,?2,?3)",
                    params![agent.id, seq, serde_json::to_string(agent)?],
                )?;
            }
            EventKind::TaskUpsert { task } => {
                let num: i64 = task.id.trim_start_matches("T-").parse().unwrap_or(0);
                tx.execute(
                    "INSERT OR REPLACE INTO tasks(id, num, data) VALUES (?1,?2,?3)",
                    params![task.id, num, serde_json::to_string(task)?],
                )?;
            }
            EventKind::Checklist { items } => {
                if let Some(a) = &ev.agent_id {
                    tx.execute(
                        "INSERT OR REPLACE INTO checklists(agent_id, data) VALUES (?1,?2)",
                        params![a, serde_json::to_string(items)?],
                    )?;
                }
            }
            EventKind::StateSet { key, value } => {
                if value.is_null() {
                    tx.execute("DELETE FROM world_state WHERE key=?1", params![key])?;
                } else {
                    tx.execute(
                        "INSERT OR REPLACE INTO world_state(key, value, updated_at) VALUES (?1,?2,?3)",
                        params![key, serde_json::to_string(value)?, ts.to_rfc3339()],
                    )?;
                }
            }
            EventKind::RunState { state } => {
                if let Some(a) = &ev.agent_id {
                    tx.execute(
                        "INSERT OR REPLACE INTO run_states(agent_id, data) VALUES (?1,?2)",
                        params![a, serde_json::to_string(state)?],
                    )?;
                }
            }
            _ => {}
        }
        tx.commit()?;
        Ok(Event { seq, ts, agent_id: ev.agent_id, visibility: ev.visibility, kind: ev.kind })
    }

    pub fn last_seq(&self) -> Result<i64> {
        Ok(self.conn().query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| r.get(0))?)
    }

    /// Events after `seq`, oldest first, at most `limit`.
    pub fn events_after(&self, seq: i64, limit: u32) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT seq, ts, agent_id, visibility, data FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )?;
        let rows = st.query_map(params![seq, limit], row_to_event)?;
        rows.map(|r| decode(r?)).collect()
    }

    /// Events of one agent's timeline after `seq`.
    pub fn agent_events(&self, agent_id: &str, after_seq: i64) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT seq, ts, agent_id, visibility, data FROM events WHERE agent_id = ?1 AND seq > ?2 ORDER BY seq",
        )?;
        let rows = st.query_map(params![agent_id, after_seq], row_to_event)?;
        rows.map(|r| decode(r?)).collect()
    }

    /// The most recent `limit` events of an agent, oldest first.
    pub fn agent_tail(&self, agent_id: &str, limit: u32) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT seq, ts, agent_id, visibility, data FROM events WHERE agent_id = ?1 ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![agent_id, limit], row_to_event)?;
        let mut v: Vec<Event> = rows.map(|r| decode(r?)).collect::<Result<_>>()?;
        v.reverse();
        Ok(v)
    }

    pub fn agents(&self) -> Result<Vec<AgentInfo>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT data FROM agents ORDER BY created_seq")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    pub fn agent(&self, id: &str) -> Result<Option<AgentInfo>> {
        let s: Option<String> = self
            .conn()
            .query_row("SELECT data FROM agents WHERE id=?1", params![id], |r| r.get(0))
            .optional()?;
        s.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }

    pub fn tasks(&self) -> Result<Vec<Task>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT data FROM tasks ORDER BY num")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    pub fn task(&self, id: &str) -> Result<Option<Task>> {
        let s: Option<String> = self
            .conn()
            .query_row("SELECT data FROM tasks WHERE id=?1", params![id], |r| r.get(0))
            .optional()?;
        s.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }

    pub fn next_task_id(&self) -> Result<String> {
        let n: i64 = self.conn().query_row("SELECT COALESCE(MAX(num),0)+1 FROM tasks", [], |r| r.get(0))?;
        Ok(format!("T-{n}"))
    }

    pub fn checklist(&self, agent_id: &str) -> Result<Vec<ChecklistItem>> {
        let s: Option<String> = self
            .conn()
            .query_row("SELECT data FROM checklists WHERE agent_id=?1", params![agent_id], |r| r.get(0))
            .optional()?;
        Ok(s.map(|s| serde_json::from_str(&s)).transpose()?.unwrap_or_default())
    }

    pub fn run_state(&self, agent_id: &str) -> Result<Option<RunState>> {
        let s: Option<String> = self
            .conn()
            .query_row("SELECT data FROM run_states WHERE agent_id=?1", params![agent_id], |r| r.get(0))
            .optional()?;
        s.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }

    /// World-state entries whose key starts with `prefix` (empty = all).
    pub fn state(&self, prefix: &str) -> Result<Vec<(String, serde_json::Value)>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT key, value FROM world_state WHERE key LIKE ?1 ESCAPE '\\' ORDER BY key")?;
        let pat = format!("{}%", prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let rows = st.query_map(params![pat], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|r| {
            let (k, v) = r?;
            Ok((k, serde_json::from_str(&v)?))
        })
        .collect()
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM meta WHERE key=?1", params![key], |r| r.get(0))
            .optional()?)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn()
            .execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?1,?2)", params![key, value])?;
        Ok(())
    }

    /// Flush the WAL into the main database file (used before migration copies).
    pub fn checkpoint(&self) -> Result<()> {
        self.conn().execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_project() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("k.db")).unwrap();
        let agent = AgentInfo {
            id: "a-1".into(),
            name: "Lead".into(),
            role_id: "assistant".into(),
            is_lead: true,
            created_by: Sender::User,
        };
        s.append(NewEvent::new(Some("a-1"), EventKind::AgentCreated { agent: agent.clone() })).unwrap();
        s.append(NewEvent::new(
            Some("a-1"),
            EventKind::Checklist { items: vec![ChecklistItem { content: "x".into(), status: ChecklistStatus::Pending }] },
        ))
        .unwrap();
        s.append(NewEvent::new(None, EventKind::StateSet { key: "hero.hp".into(), value: serde_json::json!(10) }))
            .unwrap();
        s.append(NewEvent::new(None, EventKind::StateSet { key: "hero_x".into(), value: serde_json::json!(1) }))
            .unwrap();
        assert_eq!(s.agents().unwrap(), vec![agent]);
        assert_eq!(s.checklist("a-1").unwrap().len(), 1);
        // `_` in the prefix must not act as a wildcard.
        assert_eq!(s.state("hero.").unwrap().len(), 1);
        s.append(NewEvent::new(None, EventKind::StateSet { key: "hero.hp".into(), value: serde_json::Value::Null }))
            .unwrap();
        assert_eq!(s.state("hero.").unwrap().len(), 0);
        assert_eq!(s.events_after(0, 100).unwrap().len(), 5);
        assert_eq!(s.agent_events("a-1", 0).unwrap().len(), 2);
        assert_eq!(s.next_task_id().unwrap(), "T-1");
    }
}
