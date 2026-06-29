use std::path::{Path, PathBuf};

use crate::error::{CaapError, CaapResult};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceLayout {
    root: PathBuf,
}

impl WorkspaceLayout {
    pub fn current() -> CaapResult<Self> {
        Self::from_manifest_dir(env!("CARGO_MANIFEST_DIR"))
    }

    pub fn from_manifest_dir(manifest_dir: impl AsRef<Path>) -> CaapResult<Self> {
        let root = manifest_dir
            .as_ref()
            .join("..")
            .canonicalize()
            .map_err(|error| {
                CaapError::host(format!("failed to resolve CAAP workspace root: {error}"))
            })?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}
