//! Copies the files a step declares into the run's artifact directory, so
//! a later run cannot overwrite an earlier run's evidence.

use std::path::{Path, PathBuf};

use crate::expr::{self, Scope};
use crate::report::Artifact;
use crate::spec::ArtifactSpec;

use super::RunCtx;

pub(super) fn collect(
    specs: &[ArtifactSpec],
    flow_dir: &Path,
    scope: &Scope,
    ctx: &RunCtx,
    step_id: &str,
) -> Vec<Result<Artifact, String>> {
    let mut out = Vec::new();
    let dest_dir = ctx.artifacts_dir().join(step_id.replace('/', "__"));
    for spec in specs {
        let pattern = match expr::interpolate(&spec.path, scope) {
            Ok(p) => p,
            Err(e) => {
                out.push(Err(format!("artifact {}: {e}", spec.path)));
                continue;
            }
        };
        let full = if Path::new(&pattern).is_absolute() { PathBuf::from(&pattern) } else { flow_dir.join(&pattern) };
        let matches = expand(&full);
        if matches.is_empty() {
            out.push(Err(format!("artifact {pattern}: no file matched")));
            continue;
        }
        for src in matches {
            out.push(store_file(&src, &dest_dir, &ctx.run_dir, spec.label.as_deref(), Some(&ctx.redactor)));
        }
    }
    out
}

/// Expands `*` and `?` in the last path component.
fn expand(path: &Path) -> Vec<PathBuf> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains('*') && !name.contains('?') {
        return if path.is_file() { vec![path.to_path_buf()] } else { vec![] };
    }
    let Some(dir) = path.parent() else { return vec![] };
    let Ok(glob) = globset::Glob::new(&name) else { return vec![] };
    let m = glob.compile_matcher();
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.file_name().is_some_and(|n| m.is_match(n)))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Copies a file into the run (unless it is already inside it) and
/// describes it. Text files are copied with secret values redacted. A name
/// already taken in the step's directory gets a numeric suffix.
pub fn store_file(
    src: &Path,
    dest_dir: &Path,
    run_dir: &Path,
    label: Option<&str>,
    redactor: Option<&crate::secrets::Redactor>,
) -> Result<Artifact, String> {
    let file_name = src.file_name().ok_or("no file name")?.to_string_lossy().into_owned();
    let inside =
        std::fs::canonicalize(src).ok().zip(std::fs::canonicalize(run_dir).ok()).is_some_and(|(s, r)| s.starts_with(r));
    let mime = mime_for(src);
    let is_text = mime.starts_with("text/") || mime == "application/json" || mime == "application/xml";
    let redactor = redactor.filter(|r| !r.is_empty() && is_text);
    let dest = if inside && redactor.is_none() {
        src.to_path_buf()
    } else {
        std::fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
        let mut dest = dest_dir.join(&file_name);
        if !inside || dest != src {
            let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let ext = src.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            let mut n = 1;
            while dest.exists() && std::fs::canonicalize(&dest).ok() != std::fs::canonicalize(src).ok() {
                dest = dest_dir.join(format!("{stem}-{n}{ext}"));
                n += 1;
            }
        }
        match redactor {
            Some(r) => {
                let text = std::fs::read(src).map_err(|e| format!("read {}: {e}", src.display()))?;
                let redacted = r.redact(&String::from_utf8_lossy(&text));
                std::fs::write(&dest, redacted).map_err(|e| format!("write {}: {e}", dest.display()))?;
            }
            None => {
                std::fs::copy(src, &dest).map_err(|e| format!("copy {}: {e}", src.display()))?;
            }
        }
        dest
    };
    let bytes = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    let rel = crate::catalog::rel_path(run_dir, &dest);
    Ok(Artifact { label: label.map(str::to_string).unwrap_or(file_name), path: rel, mime: mime.to_string(), bytes })
}

pub fn mime_for_name(name: &str) -> &'static str {
    mime_for(Path::new(name))
}

pub fn mime_for(path: &Path) -> &'static str {
    match path.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("vtt") => "text/vtt",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript",
        Some("css") => "text/css",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("xml") => "application/xml",
        Some("csv") => "text/csv",
        Some("md") => "text/markdown",
        Some("txt" | "log") => "text/plain",
        Some("zip") => "application/zip",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}
