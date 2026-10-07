//! Evidence bundles: a run's report and the artifacts it lists, in one zip
//! with a manifest of every file's SHA-256, for attaching to a ticket, a
//! release or an audit.
//!
//! Only `report.json` and the files the report names go in: those are what
//! Lest redacted. Anything else a step wrote into the run directory stays
//! out.

use std::io::{Read, Write};
use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::report::RunReport;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub flow_id: String,
    pub flow_name: String,
    pub run_id: String,
    pub result: String,
    pub environment: Option<String>,
    pub started_at: String,
    pub duration_ms: u64,
    pub lest_version: String,
    pub files: Vec<ManifestFile>,
    /// Listed files that could not be included, and why.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ManifestFile {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

/// The run-relative paths of every file the report names.
fn listed_files(report: &RunReport) -> Vec<String> {
    let mut out = vec!["report.json".to_string()];
    for s in report.all_steps() {
        out.extend(s.artifacts.iter().map(|a| a.path.clone()));
    }
    out.extend(report.services.iter().filter_map(|s| s.log.clone()));
    if let Some(d) = &report.demo {
        out.extend(
            [d.video.clone(), d.raw_video.clone(), d.chapters_vtt.clone(), d.beat_sheet.clone()].into_iter().flatten(),
        );
        out.extend(d.chapters.iter().filter_map(|c| c.still.clone()));
    }
    out.sort();
    out.dedup();
    out
}

/// Hashes everything read through it.
struct Hashing<R> {
    inner: R,
    hash: Sha256,
    bytes: u64,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hash.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
}

/// Zips the run into `out` (written to a temporary file, then renamed).
/// Returns the manifest.
pub fn bundle(run_dir: &Path, report: &RunReport, out: &Path) -> Result<Manifest, String> {
    let tmp = out.with_extension(format!("zip.tmp-{}", std::process::id()));
    let result = write_zip(run_dir, report, &tmp);
    match result {
        Ok(m) => {
            std::fs::rename(&tmp, out).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
            Ok(m)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn write_zip(run_dir: &Path, report: &RunReport, out: &Path) -> Result<Manifest, String> {
    let file = std::fs::File::create(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let mut manifest = Manifest {
        flow_id: report.flow_id.clone(),
        flow_name: report.flow_name.clone(),
        run_id: report.run_id.clone(),
        result: report.result.as_str().to_string(),
        environment: report.environment.clone(),
        started_at: report.started_at.clone(),
        duration_ms: report.duration_ms,
        lest_version: env!("CARGO_PKG_VERSION").to_string(),
        files: Vec::new(),
        missing: Vec::new(),
    };
    let root = std::fs::canonicalize(run_dir).map_err(|e| e.to_string())?;
    for rel in listed_files(report) {
        let path = match std::fs::canonicalize(run_dir.join(&rel)) {
            Ok(p) if p.starts_with(&root) && p.is_file() => p,
            Ok(_) => {
                manifest.missing.push(format!("{rel}: outside the run directory"));
                continue;
            }
            Err(e) => {
                manifest.missing.push(format!("{rel}: {e}"));
                continue;
            }
        };
        let f = std::fs::File::open(&path).map_err(|e| format!("{rel}: {e}"))?;
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(size >= u32::MAX as u64);
        zip.start_file(&rel, opts).map_err(|e| e.to_string())?;
        // Streamed: large videos are never held in memory.
        let mut reader = Hashing { inner: f, hash: Sha256::new(), bytes: 0 };
        std::io::copy(&mut reader, &mut zip).map_err(|e| format!("{rel}: {e}"))?;
        manifest.files.push(ManifestFile {
            path: rel,
            bytes: reader.bytes,
            sha256: hex::encode(reader.hash.finalize()),
        });
    }
    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("manifest.json", opts).map_err(|e| e.to_string())?;
    zip.write_all(&json).map_err(|e| e.to_string())?;
    zip.finish().map_err(|e| e.to_string())?;
    Ok(manifest)
}
