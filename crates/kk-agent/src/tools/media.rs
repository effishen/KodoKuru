//! Capability-engine tools: text-to-speech and image generation through
//! engines listed in the role's `engines`.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use kk_core::{EngineConfig, EngineKind, Policy};
use serde_json::{json, Value};
use std::sync::Arc;

use super::{opt_s, role_engines, s, Tool, ToolCtx};

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(TextToSpeech), Arc::new(GenerateImage)]
}

async fn pick(ctx: &ToolCtx, kind: EngineKind, wanted: Option<&str>) -> Result<EngineConfig> {
    let engines = role_engines(ctx, kind).await;
    match wanted {
        Some(w) => engines.into_iter().find(|e| e.id == w).with_context(|| format!("engine {w:?} is not available to your role")),
        None => engines.into_iter().next().with_context(|| format!("your role has no {} engine", kind.as_str())),
    }
}

fn paid_approval(ctx: &ToolCtx, e: &EngineConfig, what: String) -> Option<String> {
    (e.cost == "paid" && ctx.ws.settings().paid_engine_policy == Policy::Ask)
        .then(|| format!("Use paid engine `{}`: {what}", e.id))
}

fn write_output(ctx: &ToolCtx, path: &str, bytes: &[u8]) -> Result<String> {
    let p = kk_store::resolve_in(&ctx.ws.files_dir(), path)?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&p, bytes)?;
    Ok(format!("Wrote {path} ({} bytes)", bytes.len()))
}

struct TextToSpeech;
#[async_trait]
impl Tool for TextToSpeech {
    fn name(&self) -> &'static str {
        "text_to_speech"
    }
    fn group(&self) -> &'static str {
        "media"
    }
    fn description(&self) -> String {
        "Turn text into an audio file using a text-to-speech engine available to your role.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["text", "output_path"], "properties": {
            "text": {"type": "string"},
            "output_path": {"type": "string", "description": "e.g. audio/intro.mp3"},
            "voice": {"type": "string"},
            "format": {"type": "string", "enum": ["mp3", "wav", "opus", "flac", "aac"]},
            "engine": {"type": "string"}
        }})
    }
    async fn available(&self, ctx: &ToolCtx) -> bool {
        !role_engines(ctx, EngineKind::Tts).await.is_empty()
    }
    async fn approval(&self, ctx: &ToolCtx, a: &Value) -> Option<String> {
        let e = pick(ctx, EngineKind::Tts, opt_s(a, "engine")).await.ok()?;
        let chars = a.get("text").and_then(|t| t.as_str()).map(|t| t.chars().count()).unwrap_or(0);
        paid_approval(ctx, &e, format!("{chars} characters of speech"))
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let e = pick(ctx, EngineKind::Tts, opt_s(&a, "engine")).await?;
        let text = s(&a, "text")?;
        if text.trim().is_empty() {
            bail!("text is empty");
        }
        let voice = opt_s(&a, "voice").map(str::to_string).or(e.voice.clone()).unwrap_or_else(|| "alloy".into());
        let format = opt_s(&a, "format").unwrap_or("mp3");
        let key = ctx.ws.rt.resolve_key(&e.api_key).await?;
        let bytes = ctx.ws.rt.client.tts(&e, key.as_deref(), text, &voice, format).await.map_err(|err| anyhow::anyhow!("{err}"))?;
        write_output(ctx, s(&a, "output_path")?, &bytes)
    }
}

struct GenerateImage;
#[async_trait]
impl Tool for GenerateImage {
    fn name(&self) -> &'static str {
        "generate_image"
    }
    fn group(&self) -> &'static str {
        "media"
    }
    fn description(&self) -> String {
        "Generate an image from a prompt using an image engine available to your role.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["prompt", "output_path"], "properties": {
            "prompt": {"type": "string"},
            "output_path": {"type": "string", "description": "e.g. images/cover.png"},
            "size": {"type": "string", "description": "e.g. 1024x1024"},
            "engine": {"type": "string"}
        }})
    }
    async fn available(&self, ctx: &ToolCtx) -> bool {
        !role_engines(ctx, EngineKind::Image).await.is_empty()
    }
    async fn approval(&self, ctx: &ToolCtx, a: &Value) -> Option<String> {
        let e = pick(ctx, EngineKind::Image, opt_s(a, "engine")).await.ok()?;
        paid_approval(ctx, &e, "1 image".into())
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let e = pick(ctx, EngineKind::Image, opt_s(&a, "engine")).await?;
        let key = ctx.ws.rt.resolve_key(&e.api_key).await?;
        let bytes = ctx
            .ws
            .rt
            .client
            .image(&e, key.as_deref(), s(&a, "prompt")?, opt_s(&a, "size").unwrap_or("1024x1024"))
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        write_output(ctx, s(&a, "output_path")?, &bytes)
    }
}
