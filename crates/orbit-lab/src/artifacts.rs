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
#[derive(Clone)]
pub struct ArtifactStore {
    root: PathBuf,
    #[cfg(feature = "reliability")]
    pub(crate) hook: crate::fault::HookSlot,
}

impl std::fmt::Debug for ArtifactStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactStore").finish_non_exhaustive()
    }
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
            #[cfg(feature = "reliability")]
            hook: crate::fault::HookSlot::default(),
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
        // A concurrent `remove_stale_temporaries` can, in a narrow window
        // before the lock below is taken, delete a fresh temporary file; the
        // rename then fails with NotFound and the write is retried.
        let mut attempts = 0;
        loop {
            attempts += 1;
            match self.write_once(bytes, &digest, &path) {
                Err(LabError::Io(e))
                    if e.kind() == std::io::ErrorKind::NotFound && attempts < 3 => {}
                other => return other.map(|()| digest),
            }
        }
    }

    fn write_once(&self, bytes: &[u8], digest: &Digest, path: &Path) -> Result<()> {
        let tmp = self
            .root
            .join("tmp")
            .join(format!("{}-{}", digest.to_hex(), Uuid::now_v7()));
        let result = (|| -> Result<()> {
            let mut file = fs::File::create_new(&tmp)?;
            // Held until the file is renamed: a process that finds a
            // temporary file it can lock knows its writer is gone.
            file.lock()?;
            let half = bytes.len() / 2;
            file.write_all(&bytes[..half])?;
            #[cfg(feature = "reliability")]
            crate::fault::check(
                &self.hook,
                crate::fault::LabFaultPoint::ArtifactAfterPartialWrite,
            )?;
            file.write_all(&bytes[half..])?;
            file.sync_all()?;
            fs::rename(&tmp, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
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

    /// Removes temporary files left by writers that no longer exist (a
    /// process that died while writing). A file whose lock is still held
    /// belongs to a live writer and is kept. Returns how many were removed.
    ///
    /// # Errors
    /// I/O errors.
    pub fn remove_stale_temporaries(&self) -> Result<u64> {
        let mut removed = 0;
        for entry in fs::read_dir(self.root.join("tmp"))? {
            let path = entry?.path();
            let Ok(file) = fs::OpenOptions::new().write(true).open(&path) else {
                continue;
            };
            if file.try_lock().is_ok() {
                drop(file);
                if fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// Number of files in the temporary directory.
    ///
    /// # Errors
    /// I/O errors.
    pub fn temporary_files(&self) -> Result<u64> {
        Ok(fs::read_dir(self.root.join("tmp"))?.count() as u64)
    }

    /// Digests of every artifact file present.
    ///
    /// # Errors
    /// I/O errors.
    pub fn stored_digests(&self) -> Result<Vec<Digest>> {
        let mut out = Vec::new();
        for prefix in fs::read_dir(self.root.join("sha256"))? {
            let prefix = prefix?;
            let head = prefix.file_name().to_string_lossy().into_owned();
            if !prefix.file_type()?.is_dir() {
                continue;
            }
            for file in fs::read_dir(prefix.path())? {
                let name = file?.file_name().to_string_lossy().into_owned();
                if let Ok(d) = Digest::from_hex(&format!("{head}{name}")) {
                    out.push(d);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Location of an artifact file, for tests and local tooling. Never
    /// stored in, or returned from, portable metadata.
    #[must_use]
    pub fn local_path(&self, digest: &Digest) -> PathBuf {
        self.path_for(digest)
    }
}
