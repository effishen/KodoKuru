//! Robustness helpers for local models: inline `<think>` blocks, tool calls
//! written as text, and slightly broken JSON arguments.

use kk_core::ToolCall;
use std::sync::OnceLock;

/// Splits streamed text into reasoning (`<think>…</think>`) and answer text,
/// even when tags are split across chunks.
#[derive(Debug, Default)]
pub struct ThinkSplitter {
    buf: String,
    in_think: bool,
    seen_any: bool,
}

#[derive(Debug, PartialEq)]
pub enum Piece {
    Text(String),
    Reasoning(String),
}

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

impl ThinkSplitter {
    pub fn push(&mut self, chunk: &str) -> Vec<Piece> {
        self.buf.push_str(chunk);
        let mut out = Vec::new();
        loop {
            let tag = if self.in_think { CLOSE } else { OPEN };
            if let Some(i) = self.buf.find(tag) {
                let before: String = self.buf[..i].to_string();
                self.emit(&mut out, before);
                self.buf.drain(..i + tag.len());
                self.in_think = !self.in_think;
                continue;
            }
            // Keep a possible partial tag at the end of the buffer.
            let keep = partial_suffix(&self.buf, tag);
            let emit_to = self.buf.len() - keep;
            if emit_to > 0 {
                let text: String = self.buf[..emit_to].to_string();
                self.emit(&mut out, text);
                self.buf.drain(..emit_to);
            }
            break;
        }
        out
    }

    pub fn finish(&mut self) -> Vec<Piece> {
        let rest = std::mem::take(&mut self.buf);
        let mut out = Vec::new();
        self.emit(&mut out, rest);
        out
    }

    fn emit(&mut self, out: &mut Vec<Piece>, s: String) {
        if s.is_empty() {
            return;
        }
        if self.in_think {
            out.push(Piece::Reasoning(s));
        } else {
            // Drop leading whitespace right after a think block.
            let s = if !self.seen_any { s.trim_start().to_string() } else { s };
            if !s.is_empty() {
                self.seen_any = true;
                out.push(Piece::Text(s));
            }
        }
    }
}

fn partial_suffix(buf: &str, tag: &str) -> usize {
    let max = tag.len().saturating_sub(1).min(buf.len());
    for n in (1..=max).rev() {
        if buf.is_char_boundary(buf.len() - n) && tag.starts_with(&buf[buf.len() - n..]) {
            return n;
        }
    }
    0
}

/// Extract `<tool_call>{"name":…,"arguments":…}</tool_call>` blocks written
/// as text (Hermes/Qwen style) when the server did not parse them.
/// Returns the remaining text and the calls.
pub fn extract_text_tool_calls(content: &str) -> (String, Vec<ToolCall>) {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?s)<tool_call>\s*(.*?)\s*(?:</tool_call>|$)").unwrap());
    let mut calls = Vec::new();
    for (i, cap) in re.captures_iter(content).enumerate() {
        let body = strip_fences(cap.get(1).unwrap().as_str());
        let Some(v) = repair_json(body).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) else {
            continue;
        };
        let Some(name) = v.get("name").and_then(|n| n.as_str()) else { continue };
        let args = v.get("arguments").or_else(|| v.get("parameters")).cloned().unwrap_or(serde_json::json!({}));
        let args = match args {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        };
        calls.push(ToolCall { id: format!("call_text_{i}"), name: name.to_string(), arguments: args });
    }
    if calls.is_empty() {
        return (content.to_string(), calls);
    }
    (re.replace_all(content, "").trim().to_string(), calls)
}

fn strip_fences(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix("```json").or_else(|| s.strip_prefix("```")).unwrap_or(s);
    s.strip_suffix("```").unwrap_or(s).trim()
}

/// Try to turn almost-JSON into JSON: strip code fences, remove trailing
/// commas, close unbalanced brackets/strings. Returns `None` if hopeless.
pub fn repair_json(input: &str) -> Option<String> {
    let s = strip_fences(input);
    if s.is_empty() {
        return Some("{}".into());
    }
    if serde_json::from_str::<serde_json::Value>(s).is_ok() {
        return Some(s.to_string());
    }
    // Remove trailing commas and track nesting outside strings.
    let mut out = String::with_capacity(s.len() + 8);
    let mut stack = Vec::new();
    let mut in_str = false;
    let mut esc = false;
    for c in s.chars() {
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '{' | '[' => {
                stack.push(if c == '{' { '}' } else { ']' });
                out.push(c);
            }
            '}' | ']' => {
                let trimmed = out.trim_end();
                if trimmed.ends_with(',') {
                    let cut = trimmed.len() - 1;
                    out.truncate(cut);
                }
                stack.pop();
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    if in_str {
        out.push('"');
    }
    let trimmed = out.trim_end();
    if trimmed.ends_with(',') {
        let cut = trimmed.len() - 1;
        out.truncate(cut);
    }
    while let Some(c) = stack.pop() {
        out.push(c);
    }
    serde_json::from_str::<serde_json::Value>(&out).ok().map(|_| out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(chunks: &[&str]) -> (String, String) {
        let mut s = ThinkSplitter::default();
        let mut text = String::new();
        let mut reason = String::new();
        let mut all: Vec<Piece> = chunks.iter().flat_map(|c| s.push(c)).collect();
        all.extend(s.finish());
        for p in all {
            match p {
                Piece::Text(t) => text.push_str(&t),
                Piece::Reasoning(r) => reason.push_str(&r),
            }
        }
        (text, reason)
    }

    #[test]
    fn think_split_across_chunks() {
        let (t, r) = collect(&["<thi", "nk>plan it", "</th", "ink>\n\nHello", " world"]);
        assert_eq!(r, "plan it");
        assert_eq!(t, "Hello world");
        let (t, r) = collect(&["no thinking here"]);
        assert_eq!((t.as_str(), r.as_str()), ("no thinking here", ""));
    }

    #[test]
    fn text_tool_calls() {
        let (rest, calls) = extract_text_tool_calls(
            "Let me look.\n<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"a.txt\"}}\n</tool_call>",
        );
        assert_eq!(rest, "Let me look.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, r#"{"path":"a.txt"}"#);
    }

    #[test]
    fn repairs() {
        assert_eq!(repair_json("```json\n{\"a\":1}\n```").unwrap(), "{\"a\":1}");
        assert!(repair_json("{\"a\": [1,2,],}").is_some());
        assert!(repair_json("{\"a\": {\"b\": \"x").is_some());
        assert_eq!(repair_json("").unwrap(), "{}");
        assert!(repair_json("not json at all").is_none());
    }
}
