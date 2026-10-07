//! Evidence bundles: a run's report and artifacts in one zip, with a
//! manifest listing every file and its SHA-256, for attaching to a ticket,
//! a release or an audit.

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
}

#[derive(Debug, Serialize)]
pub struct ManifestFile {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

/// Zips the run directory into `out`. Returns the manifest.
pub fn bundle(run_dir: &Path, report: &RunReport, out: &Path) -> Result<Manifest, String> {
    let mut files: Vec<std::path::PathBuf> = walkdir::WalkDir::new(run_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();
    files.sort();
    let file = std::fs::File::create(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
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
    };
    for path in files {
        let rel = crate::catalog::rel_path(run_dir, &path);
        let mut bytes = Vec::new();
        std::fs::File::open(&path).and_then(|mut f| f.read_to_end(&mut bytes)).map_err(|e| format!("{rel}: {e}"))?;
        zip.start_file(&rel, opts).map_err(|e| e.to_string())?;
        zip.write_all(&bytes).map_err(|e| e.to_string())?;
        manifest.files.push(ManifestFile {
            sha256: hex::encode(Sha256::digest(&bytes)),
            bytes: bytes.len() as u64,
            path: rel,
        });
    }
    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    zip.start_file("manifest.json", opts).map_err(|e| e.to_string())?;
    zip.write_all(&json).map_err(|e| e.to_string())?;
    zip.finish().map_err(|e| e.to_string())?;
    Ok(manifest)
}
