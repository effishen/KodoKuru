//! File tools, confined to the workspace `files/` folder.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

use super::{opt_b, opt_s, opt_u, s, Tool, ToolCtx};

const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", ".venv", "__pycache__", "dist", "build"];
const MAX_READ_BYTES: usize = 2 * 1024 * 1024;

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ListFiles),
        Arc::new(ReadFile),
        Arc::new(WriteFile),
        Arc::new(EditFile),
        Arc::new(SearchText),
        Arc::new(MovePath),
        Arc::new(DeletePath),
    ]
}

fn resolve(ctx: &ToolCtx, p: &str) -> Result<std::path::PathBuf> {
    kk_store::resolve_in(&ctx.ws.files_dir(), p)
}

fn rel(ctx: &ToolCtx, p: &std::path::Path) -> String {
    let root = ctx.ws.files_dir().canonicalize().unwrap_or_else(|_| ctx.ws.files_dir());
    p.strip_prefix(&root).unwrap_or(p).to_string_lossy().into_owned()
}

fn human(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1}G", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1}M", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1}K", n as f64 / 1024.0),
        n => format!("{n}B"),
    }
}

struct ListFiles;
#[async_trait]
impl Tool for ListFiles {
    fn name(&self) -> &'static str {
        "list_files"
    }
    fn group(&self) -> &'static str {
        "fs"
    }
    fn description(&self) -> String {
        "List files and folders as a tree with sizes. Large dependency folders (.git, node_modules, target…) are shown but not expanded.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string", "description": "Folder relative to the workspace files (default: root)"},
            "depth": {"type": "integer", "description": "How many levels to descend (default 3, max 8)"}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let path = opt_s(&a, "path").unwrap_or(".");
        let depth = opt_u(&a, "depth").unwrap_or(3).clamp(1, 8) as usize;
        let root = resolve(ctx, path)?;
        if !root.is_dir() {
            bail!("{path:?} is not a folder");
        }
        let mut out = Vec::new();
        let mut count = 0;
        let mut it = walkdir::WalkDir::new(&root).min_depth(1).max_depth(depth).sort_by_file_name().into_iter();
        while let Some(e) = it.next() {
            let Ok(e) = e else { continue };
            count += 1;
            if count > 500 {
                out.push("… (truncated at 500 entries; list a subfolder)".to_string());
                break;
            }
            let indent = "  ".repeat(e.depth() - 1);
            let name = e.file_name().to_string_lossy();
            if e.file_type().is_dir() {
                let skip = SKIP_DIRS.contains(&name.as_ref());
                out.push(format!("{indent}{name}/{}", if skip { " (not expanded)" } else { "" }));
                if skip {
                    it.skip_current_dir();
                }
            } else {
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                out.push(format!("{indent}{name}  {}", human(size)));
            }
        }
        if out.is_empty() {
            return Ok(format!("{path} is empty"));
        }
        Ok(out.join("\n"))
    }
}

struct ReadFile;
#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn group(&self) -> &'static str {
        "fs"
    }
    fn description(&self) -> String {
        "Read a text file with line numbers. Use offset/limit to page through large files.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["path"], "properties": {
            "path": {"type": "string"},
            "offset": {"type": "integer", "description": "First line to show, 1-based (default 1)"},
            "limit": {"type": "integer", "description": "Max lines (default 400, max 2000)"}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let path = s(&a, "path")?;
        let p = resolve(ctx, path)?;
        let meta = std::fs::metadata(&p).with_context(|| format!("{path}: not found"))?;
        if meta.is_dir() {
            bail!("{path} is a folder; use list_files");
        }
        let mut f = std::fs::File::open(&p)?;
        let mut buf = Vec::new();
        use std::io::Read;
        (&mut f).take(MAX_READ_BYTES as u64).read_to_end(&mut buf)?;
        if buf.iter().take(8192).any(|b| *b == 0) {
            bail!("{path} looks binary ({} bytes); inspect it with shell tools (file, xxd, strings)", meta.len());
        }
        let text = String::from_utf8_lossy(&buf);
        let offset = opt_u(&a, "offset").unwrap_or(1).max(1) as usize;
        let limit = opt_u(&a, "limit").unwrap_or(400).clamp(1, 2000) as usize;
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        if total == 0 {
            return Ok(format!("{path} is empty"));
        }
        if offset > total {
            bail!("offset {offset} is past the end ({total} lines)");
        }
        let end = (offset - 1 + limit).min(total);
        let mut out = String::new();
        for (i, l) in lines[offset - 1..end].iter().enumerate() {
            let l = if l.len() > 2000 { kk_llm::truncate(l, 2000) } else { l.to_string() };
            out.push_str(&format!("{:>5}│ {l}\n", offset + i));
        }
        if end < total || meta.len() as usize > MAX_READ_BYTES {
            out.push_str(&format!("… showing lines {offset}-{end} of {total}; use offset to read more\n"));
        }
        Ok(out)
    }
}

struct WriteFile;
#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn group(&self) -> &'static str {
        "fs"
    }
    fn description(&self) -> String {
        "Create or overwrite a file with the given content (parent folders are created). For small changes to an existing file prefer edit_file.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["path", "content"], "properties": {
            "path": {"type": "string"},
            "content": {"type": "string"}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let path = s(&a, "path")?;
        let content = s(&a, "content")?;
        let p = resolve(ctx, path)?;
        if p.is_dir() {
            bail!("{path} is a folder");
        }
        let existed = p.exists();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, content)?;
        Ok(format!(
            "{} {path} ({} bytes, {} lines)",
            if existed { "Overwrote" } else { "Created" },
            content.len(),
            content.lines().count()
        ))
    }
}

struct EditFile;
#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &'static str {
        "edit_file"
    }
    fn group(&self) -> &'static str {
        "fs"
    }
    fn description(&self) -> String {
        "Replace an exact piece of text in a file. old_string must match exactly (including whitespace) and be unique unless replace_all is true. Read the file first.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["path", "old_string", "new_string"], "properties": {
            "path": {"type": "string"},
            "old_string": {"type": "string"},
            "new_string": {"type": "string"},
            "replace_all": {"type": "boolean"}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let path = s(&a, "path")?;
        let old = s(&a, "old_string")?;
        let new = s(&a, "new_string")?;
        if old.is_empty() {
            bail!("old_string is empty; use write_file to create a file");
        }
        let p = resolve(ctx, path)?;
        let text = std::fs::read_to_string(&p).with_context(|| format!("{path}: cannot read"))?;
        let n = text.matches(old).count();
        let all = opt_b(&a, "replace_all").unwrap_or(false);
        if n == 0 {
            bail!("old_string was not found in {path}. Read the file and copy the text exactly.");
        }
        if n > 1 && !all {
            bail!("old_string occurs {n} times in {path}; include more surrounding text to make it unique, or set replace_all");
        }
        let out = if all { text.replace(old, new) } else { text.replacen(old, new, 1) };
        std::fs::write(&p, &out)?;
        let line = out.find(new).map(|i| out[..i].lines().count() + 1).unwrap_or(0);
        Ok(format!("Replaced {} occurrence(s) in {path} (near line {line})", if all { n } else { 1 }))
    }
}

struct SearchText;
#[async_trait]
impl Tool for SearchText {
    fn name(&self) -> &'static str {
        "search_text"
    }
    fn group(&self) -> &'static str {
        "fs"
    }
    fn description(&self) -> String {
        "Search files for a regular expression. Returns file:line: text. Skips binary files and dependency folders.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["pattern"], "properties": {
            "pattern": {"type": "string", "description": "Rust regex syntax"},
            "path": {"type": "string", "description": "Folder or file to search (default root)"},
            "glob": {"type": "string", "description": "Only files matching this glob, e.g. **/*.rs"},
            "ignore_case": {"type": "boolean"},
            "max_results": {"type": "integer", "description": "default 100"}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let pattern = s(&a, "pattern")?;
        let re = regex::RegexBuilder::new(pattern)
            .case_insensitive(opt_b(&a, "ignore_case").unwrap_or(false))
            .size_limit(1 << 20)
            .build()
            .context("invalid regex")?;
        let root = resolve(ctx, opt_s(&a, "path").unwrap_or("."))?;
        let glob = match opt_s(&a, "glob") {
            Some(g) => Some(globset::Glob::new(g).context("invalid glob")?.compile_matcher()),
            None => None,
        };
        let max = opt_u(&a, "max_results").unwrap_or(100).clamp(1, 1000) as usize;
        let files_root = ctx.ws.files_dir().canonicalize()?;
        let mut hits = Vec::new();
        let walker = walkdir::WalkDir::new(&root).into_iter().filter_entry(|e| {
            !(e.file_type().is_dir() && SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref()) && e.depth() > 0)
        });
        'files: for e in walker.flatten() {
            if !e.file_type().is_file() || e.metadata().map(|m| m.len() > 8 << 20).unwrap_or(true) {
                continue;
            }
            let relp = e.path().strip_prefix(&files_root).unwrap_or(e.path());
            if let Some(g) = &glob {
                if !g.is_match(relp) {
                    continue;
                }
            }
            let Ok(bytes) = std::fs::read(e.path()) else { continue };
            if bytes.iter().take(4096).any(|b| *b == 0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    hits.push(format!("{}:{}: {}", relp.display(), i + 1, kk_llm::truncate(line.trim(), 300)));
                    if hits.len() >= max {
                        hits.push(format!("… stopped at {max} results"));
                        break 'files;
                    }
                }
            }
        }
        if hits.is_empty() {
            return Ok("No matches.".into());
        }
        Ok(hits.join("\n"))
    }
}

struct MovePath;
#[async_trait]
impl Tool for MovePath {
    fn name(&self) -> &'static str {
        "move_path"
    }
    fn group(&self) -> &'static str {
        "fs"
    }
    fn description(&self) -> String {
        "Move or rename a file or folder inside the workspace.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["from", "to"], "properties": {
            "from": {"type": "string"}, "to": {"type": "string"}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let from = resolve(ctx, s(&a, "from")?)?;
        let to = resolve(ctx, s(&a, "to")?)?;
        if !from.exists() {
            bail!("{} does not exist", s(&a, "from")?);
        }
        if to.exists() {
            bail!("{} already exists", s(&a, "to")?);
        }
        if let Some(d) = to.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::rename(&from, &to)?;
        Ok(format!("Moved {} → {}", rel(ctx, &from), rel(ctx, &to)))
    }
}

struct DeletePath;
#[async_trait]
impl Tool for DeletePath {
    fn name(&self) -> &'static str {
        "delete_path"
    }
    fn group(&self) -> &'static str {
        "fs"
    }
    fn description(&self) -> String {
        "Delete a file, or a folder with recursive=true. Cannot be undone.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "required": ["path"], "properties": {
            "path": {"type": "string"}, "recursive": {"type": "boolean"}
        }})
    }
    async fn run(&self, ctx: &ToolCtx, a: Value) -> Result<String> {
        let path = s(&a, "path")?;
        let p = resolve(ctx, path)?;
        let root = ctx.ws.files_dir().canonicalize()?;
        if p == root {
            bail!("refusing to delete the workspace root");
        }
        if p.is_dir() {
            if !opt_b(&a, "recursive").unwrap_or(false) {
                bail!("{path} is a folder; set recursive=true to delete it and its contents");
            }
            std::fs::remove_dir_all(&p)?;
        } else {
            std::fs::remove_file(&p).with_context(|| format!("{path}: not found"))?;
        }
        Ok(format!("Deleted {path}"))
    }
}
