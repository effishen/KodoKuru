//! Structured world state (for games, simulations, long investigations)
//! and dice.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use kk_core::EventKind;
use rand::Rng;
use serde_json::{json, Value};
use std::sync::Arc;

use super::{opt_s, s, Tool, ToolCtx};

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(StateGet), Arc::new(StateSet), Arc::new(RollDice)]
}

struct StateGet;
#[async_trait]
impl Tool for StateGet {
    fn name(&self) -> &'static str {
        "state_get"
    }
    fn group(&self) -> &'static str {
        "state"
    }
    fn description(&self) -> String {
        "Read the world state: one key, all keys with a prefix (e.g. `npc.`), or everything. Values are JSON.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "key": {"type": "string"},
            "prefix": {"type": "string"}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let store = ctx.ws.store();
        if let Some(k) = opt_s(&a, "key") {
            let v = store.state(k)?.into_iter().find(|(key, _)| key == k).map(|(_, v)| v);
            return Ok(match v {
                Some(v) => format!("{k} = {v}"),
                None => format!("{k} is not set"),
            });
        }
        let entries = store.state(opt_s(&a, "prefix").unwrap_or(""))?;
        if entries.is_empty() {
            return Ok("No matching state.".into());
        }
        let out: serde_json::Map<String, Value> = entries.into_iter().collect();
        Ok(serde_json::to_string_pretty(&out)?)
    }
}

struct StateSet;
#[async_trait]
impl Tool for StateSet {
    fn name(&self) -> &'static str {
        "state_set"
    }
    fn group(&self) -> &'static str {
        "state"
    }
    fn description(&self) -> String {
        "Set a world-state key to a JSON value (null deletes it). Use dotted keys like `player.hp`, `location.cave.explored`, `secret.killer`.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["key", "value"], "properties": {
            "key": {"type": "string"},
            "value": {"description": "any JSON value"}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let key = s(&a, "key")?.trim().to_string();
        if key.is_empty() || key.len() > 200 {
            bail!("key must be 1-200 characters");
        }
        let value = a.get("value").cloned().context("missing `value`")?;
        ctx.ws.emit(Some(&ctx.agent.id), EventKind::StateSet { key: key.clone(), value: value.clone() })?;
        Ok(if value.is_null() { format!("Deleted {key}") } else { format!("{key} = {value}") })
    }
}

/// Roll dice like `2d6+1`, `d20`, `4d6-2`.
pub fn roll(expr: &str, rng: &mut impl Rng) -> Result<(Vec<u32>, i64)> {
    let e = expr.replace(' ', "").to_lowercase();
    let re = regex::Regex::new(r"^(\d*)d(\d+)([+-]\d+)?$").unwrap();
    let c = re.captures(&e).with_context(|| format!("bad dice expression {expr:?}; use e.g. 2d6+1"))?;
    let n: u32 = c.get(1).map(|m| m.as_str()).filter(|s| !s.is_empty()).unwrap_or("1").parse()?;
    let sides: u32 = c[2].parse()?;
    let modifier: i64 = c.get(3).map(|m| m.as_str().parse()).transpose()?.unwrap_or(0);
    if !(1..=100).contains(&n) || !(2..=1000).contains(&sides) {
        bail!("1-100 dice with 2-1000 sides");
    }
    let rolls: Vec<u32> = (0..n).map(|_| rng.gen_range(1..=sides)).collect();
    let total = rolls.iter().map(|r| *r as i64).sum::<i64>() + modifier;
    Ok((rolls, total))
}

struct RollDice;
#[async_trait]
impl Tool for RollDice {
    fn name(&self) -> &'static str {
        "roll_dice"
    }
    fn group(&self) -> &'static str {
        "state"
    }
    fn description(&self) -> String {
        "Roll dice (e.g. `d20`, `2d6+1`). The result is random and recorded; respect it.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["dice"], "properties": {
            "dice": {"type": "string"},
            "reason": {"type": "string", "description": "what the roll is for"}
        }})
    }
    async fn run(&self, _ctx: &ToolCtx, a: Value) -> Result<String> {
        let expr = s(&a, "dice")?;
        let (rolls, total) = roll(expr, &mut rand::thread_rng())?;
        Ok(format!(
            "{}{expr}: {:?} → {total}",
            opt_s(&a, "reason").map(|r| format!("{r} — ")).unwrap_or_default(),
            rolls
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dice() {
        let mut rng = rand::thread_rng();
        for _ in 0..100 {
            let (r, t) = roll("2d6+1", &mut rng).unwrap();
            assert_eq!(r.len(), 2);
            assert!((3..=13).contains(&t));
        }
        assert_eq!(roll("d20", &mut rng).unwrap().0.len(), 1);
        assert!(roll("lots", &mut rng).is_err());
        assert!(roll("0d6", &mut rng).is_err());
    }
}
