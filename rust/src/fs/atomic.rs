//! Crash-safe file writes: sha256 hashing, temp-file + rename writes, and
//! startup recovery of leftover temp files.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::Result;

/// SHA-256 hex digest of string or binary content.
pub fn hash_content(data: impl AsRef<[u8]>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_ref());
    hex::encode(hasher.finalize())
}

/// Write `data` to `path` atomically: content lands in a hidden `.<ulid>.tmp`
/// sibling file first, then a single `rename` publishes it.
pub fn atomic_write(path: impl AsRef<Path>, data: impl AsRef<[u8]>) -> Result<()> {
    let path = path.as_ref();
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp: PathBuf = dir.join(format!(".{}.tmp", ulid::Ulid::new()));

    let write = || -> std::io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(data.as_ref())?;
        file.flush()?;
        Ok(())
    };
    if let Err(err) = write() {
        let _ = fs::remove_file(&tmp);
        return Err(err.into());
    }
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err.into());
    }
    Ok(())
}

/// Result of [`read_file_atomic`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAtomic {
    pub content: String,
    pub hash: String,
}

/// Read a file as UTF-8 together with its content hash.
pub fn read_file_atomic(path: impl AsRef<Path>) -> Result<FileAtomic> {
    let content = fs::read_to_string(path)?;
    let hash = hash_content(&content);
    Ok(FileAtomic { content, hash })
}

/// Delete a file, treating a missing file as success.
pub fn remove_if_exists(path: impl AsRef<Path>) -> Result<()> {
    if let Err(err) = fs::remove_file(path.as_ref()) {
        if err.kind() != std::io::ErrorKind::NotFound {
            return Err(err.into());
        }
    }
    Ok(())
}

/// Remove leftover `*.tmp` files in `dir` (recovery from crashed writes).
/// Best-effort: unreadable entries and races are ignored.
pub fn cleanup_temp_files(dir: impl AsRef<Path>) {
    let entries = match fs::read_dir(dir.as_ref()) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".tmp") {
            continue;
        }
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_hex() {
        assert_eq!(hash_content("abc"), hash_content("abc"));
        assert_ne!(hash_content("abc"), hash_content("abd"));
        assert_eq!(hash_content("").len(), 64);
    }
}
