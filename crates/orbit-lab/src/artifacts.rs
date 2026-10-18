//! Content-addressed artifact files.
//!
//! Artifact bytes live under `<lab dir>/artifacts/sha256/<first 2 hex>/<remaining 62 hex>`.
//! The path is derived only from a [`Digest`], never from caller input, so an
//! experiment request cannot name a file-system location. Registry rows
//! record each artifact's digest, size and media type; they never contain
//! a path.
//!
//! Writes go to a temporary file in the same directory tree, are flushed
//! with `fsync`, and are then renamed into place, so a reader never sees a
//! partial artifact under its final name. Reads always re-hash the content
//! and refuse it on mismatch: an artifact is trusted only after it has
//! been verified.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use orbit_core::Digest;
use serde::Serialize;
use uuid::Uuid;

use crate::digest::content_digest;
use crate::error::{LabError, Result};

/// State of an artifact file compared with its recorded digest and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactState {
    /// Present, with the recorded size and digest.
    Intact,
    /// No regular file at the expected location.
    Missing,
    /// Present, but its size or content differs from the record.
    Corrupted,
}

/// The artifact directory.
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    /// Opens (creating if needed) the artifact store rooted at `root`.
    ///
    /// # Errors
    /// I/O errors.
    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root.join("sha256"))?;
        fs::create_dir_all(root.join("tmp"))?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    fn path_for(&self, digest: &Digest) -> PathBuf {
        let hex = digest.to_hex();
        self.root.join("sha256").join(&hex[..2]).join(&hex[2..])
    }

    /// Stores `bytes` and returns their digest. Storing the same bytes twice
    /// keeps one file. An existing file that fails verification is replaced.
    ///
    /// # Errors
    /// I/O errors.
    pub fn put(&self, bytes: &[u8]) -> Result<Digest> {
        let digest = content_digest(bytes);
        let size = bytes.len() as u64;
        if self.check(&digest, size)? == ArtifactState::Intact {
            return Ok(digest);
        }
        let path = self.path_for(&digest);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = self
            .root
            .join("tmp")
            .join(format!("{}-{}", digest.to_hex(), Uuid::now_v7()));
        let result = (|| {
            let mut file = fs::File::create_new(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&tmp, &path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        Ok(digest)
    }

    /// Compares the file for `digest` with the recorded `size` and digest.
    /// Symbolic links are treated as missing and never followed.
    ///
    /// # Errors
    /// I/O errors other than "not found".
    pub fn check(&self, digest: &Digest, size: u64) -> Result<ArtifactState> {
        match self.read_unverified(digest)? {
            None => Ok(ArtifactState::Missing),
            Some(bytes) if bytes.len() as u64 == size && content_digest(&bytes) == *digest => {
                Ok(ArtifactState::Intact)
            }
            Some(_) => Ok(ArtifactState::Corrupted),
        }
    }

    /// Reads and verifies an artifact.
    ///
    /// # Errors
    /// [`LabError::ArtifactMissing`] or [`LabError::ArtifactCorrupted`].
    pub fn read(&self, digest: &Digest) -> Result<Vec<u8>> {
        let bytes = self
            .read_unverified(digest)?
            .ok_or(LabError::ArtifactMissing(*digest))?;
        if content_digest(&bytes) == *digest {
            Ok(bytes)
        } else {
            Err(LabError::ArtifactCorrupted(*digest))
        }
    }

    fn read_unverified(&self, digest: &Digest) -> Result<Option<Vec<u8>>> {
        let path = self.path_for(digest);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => Ok(Some(fs::read(&path)?)),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Location of an artifact file, for tests and local tooling. Never
    /// stored in, or returned from, portable metadata.
    #[must_use]
    pub fn local_path(&self, digest: &Digest) -> PathBuf {
        self.path_for(digest)
    }
}
