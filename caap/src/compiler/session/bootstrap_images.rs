//! Bootstrap image and execution-trace management for [`Compiler`].
//!
//! Storing/restoring deterministic bootstrap images (units + capabilities +
//! fact schema + base semantic entries), loading them from trusted files, and
//! recording bootstrap execution/trace events. Split out of `session/mod.rs` so
//! the composition root keeps only core session accessors and service entry
//! points.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::rc::Rc;

use crate::artifacts::ArtifactFingerprint;
use crate::compiler::bootstrap::{
    BootstrapCapabilityGraph, BootstrapImage, BootstrapImageFile, BootstrapImageStore,
    BootstrapImageTrustPolicy, BootstrapTraceEvent,
};
use crate::error::{CaapError, CaapResult};
use crate::unit::Unit;

use super::Compiler;

impl Compiler {
    pub fn has_bootstrap_executions(&self) -> bool {
        !self.bootstrap.executions.borrow().is_empty()
    }

    pub fn bootstrap_executions(&self) -> Vec<String> {
        self.bootstrap.executions.borrow().clone()
    }

    pub fn bootstrap_trace(&self) -> Vec<BootstrapTraceEvent> {
        self.bootstrap.trace.borrow().clone()
    }

    pub fn bootstrap_capabilities(&self) -> &BootstrapCapabilityGraph {
        &self.bootstrap.capabilities
    }

    pub fn bootstrap_images(&self) -> &BootstrapImageStore {
        &self.bootstrap.images
    }

    pub fn store_bootstrap_image(&mut self, name: impl Into<String>) -> CaapResult<BootstrapImage> {
        let name = name.into();
        if name.is_empty() {
            return Err(CaapError::compiler(
                "bootstrap image name must be non-empty",
            ));
        }
        let image = BootstrapImage {
            name,
            units: {
                let mut units: Vec<_> = self.units.values().map(Unit::to_template).collect();
                units.sort_by(|left, right| left.unit_id.cmp(&right.unit_id));
                units
            },
            capabilities: self.bootstrap.capabilities.clone(),
            fact_schema: (*self.fact_schema).clone(),
            base_semantic_entries: {
                let mut entries: Vec<_> = self.base_semantic_entries.values().cloned().collect();
                entries.sort_by(|left, right| left.name.cmp(&right.name));
                entries
            },
            session_version: self.session_version,
        };
        self.bootstrap.images.store(image.clone())?;
        self.advance_session_version()?;
        Ok(image)
    }

    pub fn restore_bootstrap_image(&mut self, name: &str) -> CaapResult<()> {
        let image = self.bootstrap.images.get(name)?.cloned().ok_or_else(|| {
            CaapError::compiler(format!("bootstrap image does not exist: {name}"))
        })?;
        let mut units = BTreeMap::new();
        for template in image.units {
            let unit = Unit::from_template(template)?;
            units.insert(unit.unit_id().to_string(), unit);
        }
        self.units = Rc::new(units);
        self.bootstrap.capabilities = image.capabilities;
        self.fact_schema = Rc::new(image.fact_schema);
        self.base_semantic_entries = Rc::new(
            image
                .base_semantic_entries
                .into_iter()
                .map(|entry| (entry.name.clone(), entry))
                .collect(),
        );
        self.advance_unit_registry_version()?;
        Ok(())
    }

    pub fn save_bootstrap_image_file(
        &mut self,
        name: &str,
        path: impl AsRef<Path>,
    ) -> CaapResult<()> {
        let path = path.as_ref();
        self.bootstrap.images.save_image_file(name, path)?;
        self.emit_compiler_event(
            "bootstrap.image.save",
            Some(name.to_string()),
            "saved bootstrap image file",
            [("path".to_string(), path.display().to_string())],
        )?;
        Ok(())
    }

    pub fn load_bootstrap_image_file(&mut self, path: impl AsRef<Path>) -> CaapResult<String> {
        let path = path.as_ref();
        let image_file = BootstrapImageFile::read_json_file(path)?;
        let image_name = image_file.image.name.clone();
        self.bootstrap.images.restore_image_file(image_file)?;
        self.emit_compiler_event(
            "bootstrap.image.load",
            Some(image_name.clone()),
            "loaded bootstrap image file",
            [("path".to_string(), path.display().to_string())],
        )?;
        Ok(image_name)
    }

    pub fn load_trusted_bootstrap_image_file(
        &mut self,
        path: impl AsRef<Path>,
        trust_policy: &BootstrapImageTrustPolicy,
    ) -> CaapResult<String> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|error| {
            CaapError::compiler(format!(
                "failed to read bootstrap image file {}: {error}",
                path.display()
            ))
        })?;
        let fingerprint = ArtifactFingerprint::sha256(text.as_bytes()).to_string();
        trust_policy.require_fingerprint(&fingerprint)?;
        let image_file = BootstrapImageFile::from_json_str(&text)?;
        let image_name = image_file.image.name.clone();
        self.bootstrap.images.restore_image_file(image_file)?;
        self.emit_compiler_event(
            "bootstrap.image.load",
            Some(image_name.clone()),
            "loaded trusted bootstrap image file",
            [
                ("fingerprint".to_string(), fingerprint),
                ("path".to_string(), path.display().to_string()),
                ("trusted".to_string(), "true".to_string()),
            ],
        )?;
        Ok(image_name)
    }

    pub fn record_bootstrap_execution(&mut self, path: impl Into<String>) -> CaapResult<()> {
        let path = path.into();
        if path.is_empty() {
            return Err(CaapError::compiler(
                "bootstrap execution path must be non-empty",
            ));
        }
        self.bootstrap.executions.borrow_mut().push(path);
        self.advance_session_version()?;
        Ok(())
    }

    pub(in crate::compiler) fn push_bootstrap_trace(
        &mut self,
        action: impl Into<String>,
        target: impl Into<String>,
        depth: usize,
        succeeded: bool,
    ) -> CaapResult<()> {
        let action = action.into();
        let target = target.into();
        if action.is_empty() {
            return Err(CaapError::compiler(
                "bootstrap trace action must be non-empty",
            ));
        }
        if target.is_empty() {
            return Err(CaapError::compiler(
                "bootstrap trace target must be non-empty",
            ));
        }
        self.bootstrap.trace.borrow_mut().push(BootstrapTraceEvent {
            action,
            target,
            depth,
            succeeded,
        });
        Ok(())
    }
}
