//! Bundle export/import: tar.gz of the wiki root, optionally filtered.

use crate::error::{Error, Result};
use flate2::write::GzEncoder;
use std::path::Path;

pub fn export_files(root: &str, rel_paths: Option<&[String]>) -> Result<Vec<u8>> {
    let root_path = Path::new(root);
    let gz = GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);

    match rel_paths {
        None => {
            tar.append_dir_all(".", root_path)
                .map_err(|e| Error::Other(e.to_string()))?;
        }
        Some(paths) => {
            for rel in paths {
                let full = root_path.join(rel);
                if !full.is_file() {
                    continue;
                }
                let data = std::fs::read(&full)?;
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append_data(&mut header, rel, data.as_slice())
                    .map_err(|e| Error::Other(e.to_string()))?;
            }
        }
    }
    let gz = tar.into_inner()?;
    Ok(gz.finish()?)
}

/// Guard: reject absolute paths and `..` escapes.
pub fn entry_is_safe(entry_path: &str) -> bool {
    if entry_path.starts_with('/') || entry_path.contains('\\') {
        return false;
    }
    !entry_path.split('/').any(|seg| seg == "..")
}

pub fn import_bytes(root: &str, buf: &[u8], force: bool) -> Result<(usize, Vec<String>)> {
    use std::io::Read;
    let gz = flate2::read::GzDecoder::new(buf);
    let mut archive = tar::Archive::new(gz);

    let root_path = Path::new(root);
    // header pass: conflict detection
    let mut conflicts = Vec::new();
    let mut names = Vec::new();
    for entry in archive.entries().map_err(|e| Error::Other(e.to_string()))? {
        let mut entry = entry.map_err(|e| Error::Other(e.to_string()))?;
        let path = entry
            .path()
            .map_err(|e| Error::Other(e.to_string()))?
            .to_string_lossy()
            .to_string();
        if !entry_is_safe(&path) {
            return Err(Error::Validation(format!("unsafe archive entry: {path}")));
        }
        if path.ends_with('/') {
            continue;
        }
        if root_path.join(&path).exists() {
            conflicts.push(path);
        } else {
            names.push(path);
        }
        let _ = entry.header();
        // consume data so iterator advances
        let mut sink = Vec::new();
        let _ = entry.read_to_end(&mut sink);
    }

    // second pass over the same buffer for extraction
    if !conflicts.is_empty() && !force {
        return Ok((0, conflicts));
    }
    let gz = flate2::read::GzDecoder::new(buf);
    let mut archive = tar::Archive::new(gz);
    let mut imported = 0usize;
    for entry in archive.entries().map_err(|e| Error::Other(e.to_string()))? {
        let mut entry = entry.map_err(|e| Error::Other(e.to_string()))?;
        let path = entry
            .path()
            .map_err(|e| Error::Other(e.to_string()))?
            .to_string_lossy()
            .to_string();
        if !entry_is_safe(&path) {
            continue;
        }
        let target = root_path.join(&path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&target)?;
        std::io::copy(&mut entry, &mut out)?;
        imported += 1;
    }
    Ok((imported, Vec::new()))
}
