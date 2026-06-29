use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::diagnostics::Diagnostic;
use crate::error::{CaapError, CaapResult};
use crate::semantic::{CapabilityName, PhasePolicy, SemanticEntry};
use crate::unit::{Unit, UnitTemplate};
use crate::values::{EvalResult, EvalSignal, RuntimeValue};

use super::fact_schema::FactSchemaRegistry;
use super::query_provider::normalize_virtual_path;
use super::session::{
    bootstrap_image_file_fingerprint, elapsed_ms_string, path_to_string, resolve_source_path,
    validate_base_semantic_entry, Compiler,
};

pub struct CompilerBootstrapController<'a> {
    pub(super) compiler: &'a mut Compiler,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct BootstrapVirtualFileSystem {
    files: BTreeMap<String, String>,
    version: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct BootstrapCapabilityGraph {
    grants: BTreeMap<String, BTreeSet<CapabilityName>>,
    version: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct BootstrapImage {
    pub name: String,
    pub units: Vec<UnitTemplate>,
    pub capabilities: BootstrapCapabilityGraph,
    #[serde(default)]
    pub fact_schema: FactSchemaRegistry,
    #[serde(default)]
    pub base_semantic_entries: Vec<SemanticEntry>,
    pub session_version: u64,
}

#[derive(Clone, Debug, Default)]
pub struct BootstrapImageStore {
    images: BTreeMap<String, BootstrapImage>,
    version: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct BootstrapImageFile {
    pub format_name: String,
    pub format_version: u32,
    pub image: BootstrapImage,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BootstrapImageTrustPolicy {
    trusted_fingerprints: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub struct EvaluationCapture {
    pub unit_id: String,
    pub phase: PhasePolicy,
    pub value: Option<RuntimeValue>,
    pub bindings: Vec<(String, RuntimeValue)>,
    pub diagnostics: Vec<Diagnostic>,
    pub skipped_forms: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootstrapTraceEvent {
    pub action: String,
    pub target: String,
    pub depth: usize,
    pub succeeded: bool,
}

pub(super) fn normalize_bootstrap_capabilities<T>(
    capabilities: impl IntoIterator<Item = T>,
) -> CaapResult<Vec<CapabilityName>>
where
    T: Into<String>,
{
    let mut normalized = Vec::new();
    for capability in capabilities {
        let capability = capability.into();
        // Accept explicit capability names from any host domain. The kernel
        // validates shape but does not hard-code feature domains; `host_services`
        // is rejected because it is an obsolete universal alias.
        if capability == "host_services" {
            return Err(CaapError::compiler(format!(
                "unsupported bootstrap internal capability: {capability}; use explicit capability names such as sys or sys.fs.read"
            )));
        }
        let capability = CapabilityName::new(&capability).map_err(|error| {
            CaapError::compiler(format!(
                "bootstrap internal capability name is invalid: {error}"
            ))
        })?;
        if !normalized.contains(&capability) {
            normalized.push(capability);
        }
    }
    normalized.sort();
    Ok(normalized)
}

fn validate_bootstrap_capability_name(capability: &str) -> CaapResult<()> {
    CapabilityName::new(capability)
        .map(|_| ())
        .map_err(|error| {
            CaapError::compiler(format!("bootstrap capability name is invalid: {error}"))
        })
}

pub(super) fn bootstrap_execution_memo_key(
    action: &str,
    target: &str,
    fingerprint: &str,
    capabilities: &[CapabilityName],
) -> String {
    let mut key = String::new();
    for segment in [action, target, fingerprint] {
        append_memo_key_segment(&mut key, segment);
    }
    for capability in capabilities {
        append_memo_key_segment(&mut key, capability.as_str());
    }
    key
}

fn append_memo_key_segment(key: &mut String, segment: &str) {
    key.push_str(&segment.len().to_string());
    key.push(':');
    key.push_str(segment);
}

impl BootstrapVirtualFileSystem {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, path: impl Into<String>, text: impl Into<String>) -> CaapResult<()> {
        let path = normalize_virtual_path(path.into())?;
        let text = text.into();
        if self.files.get(&path) != Some(&text) {
            let version = self.next_version()?;
            self.files.insert(path, text);
            self.version = version;
        }
        Ok(())
    }

    pub fn read(&self, path: &str) -> CaapResult<&str> {
        let path = normalize_virtual_path(path.to_string())?;
        self.files.get(&path).map(String::as_str).ok_or_else(|| {
            CaapError::compiler(format!("virtual bootstrap file does not exist: {path}"))
        })
    }

    pub fn contains(&self, path: &str) -> bool {
        normalize_virtual_path(path.to_string())
            .ok()
            .is_some_and(|path| self.files.contains_key(&path))
    }

    pub fn paths(&self) -> Vec<&str> {
        self.files.keys().map(String::as_str).collect()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    fn next_version(&self) -> CaapResult<u64> {
        self.version
            .checked_add(1)
            .ok_or_else(|| CaapError::compiler("bootstrap virtual file system version overflow"))
    }
}

impl<'de> Deserialize<'de> for BootstrapVirtualFileSystem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct BootstrapVirtualFileSystemData {
            files: BTreeMap<String, String>,
            version: u64,
        }

        let data = BootstrapVirtualFileSystemData::deserialize(deserializer)?;
        let mut files = BTreeMap::new();
        for (path, text) in data.files {
            let path = normalize_virtual_path(path).map_err(serde::de::Error::custom)?;
            if files.insert(path.clone(), text).is_some() {
                return Err(serde::de::Error::custom(format!(
                    "duplicate virtual bootstrap path after normalization: {path}"
                )));
            }
        }
        Ok(Self {
            files,
            version: data.version,
        })
    }
}

impl BootstrapCapabilityGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn grant(
        &mut self,
        unit_id: impl Into<String>,
        capability: impl Into<String>,
    ) -> CaapResult<bool> {
        let unit_id = unit_id.into();
        let capability = capability.into();
        if unit_id.is_empty() {
            return Err(CaapError::compiler(
                "bootstrap capability unit id must be non-empty",
            ));
        }
        let capability = CapabilityName::new(&capability).map_err(|error| {
            CaapError::compiler(format!("bootstrap capability name is invalid: {error}"))
        })?;
        let inserted = !self
            .grants
            .get(&unit_id)
            .is_some_and(|capabilities| capabilities.contains(&capability));
        if inserted {
            let version = self.next_version()?;
            self.grants.entry(unit_id).or_default().insert(capability);
            self.version = version;
        }
        Ok(inserted)
    }

    pub fn grant_many(
        &mut self,
        unit_id: impl Into<String>,
        capabilities: impl IntoIterator<Item = String>,
    ) -> CaapResult<bool> {
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err(CaapError::compiler(
                "bootstrap capability unit id must be non-empty",
            ));
        }
        let mut normalized = BTreeSet::new();
        for capability in capabilities {
            let capability = CapabilityName::new(&capability).map_err(|error| {
                CaapError::compiler(format!("bootstrap capability name is invalid: {error}"))
            })?;
            normalized.insert(capability);
        }
        let additions: Vec<_> = normalized
            .into_iter()
            .filter(|capability| {
                !self
                    .grants
                    .get(&unit_id)
                    .is_some_and(|existing| existing.contains(capability))
            })
            .collect();
        if additions.is_empty() {
            return Ok(false);
        }
        let version = self.next_version()?;
        self.grants.entry(unit_id).or_default().extend(additions);
        self.version = version;
        Ok(true)
    }

    pub fn revoke(&mut self, unit_id: &str, capability: &str) -> CaapResult<bool> {
        if unit_id.is_empty() {
            return Err(CaapError::compiler(
                "bootstrap capability unit id must be non-empty",
            ));
        }
        validate_bootstrap_capability_name(capability)?;
        let capability_name = CapabilityName::new(capability).map_err(|error| {
            CaapError::compiler(format!("bootstrap capability name is invalid: {error}"))
        })?;
        let Some(capabilities) = self.grants.get(unit_id) else {
            return Ok(false);
        };
        if !capabilities.contains(&capability_name) {
            return Ok(false);
        }
        let version = self.next_version()?;
        let Some(capabilities) = self.grants.get_mut(unit_id) else {
            return Ok(false);
        };
        capabilities.remove(capability_name.as_str());
        if capabilities.is_empty() {
            self.grants.remove(unit_id);
        }
        self.version = version;
        Ok(true)
    }

    pub fn revoke_many<'b>(
        &mut self,
        unit_id: &str,
        capabilities: impl IntoIterator<Item = &'b str>,
    ) -> CaapResult<bool> {
        if unit_id.is_empty() {
            return Err(CaapError::compiler(
                "bootstrap capability unit id must be non-empty",
            ));
        }
        let mut normalized = Vec::new();
        for capability in capabilities {
            validate_bootstrap_capability_name(capability)?;
            normalized.push(CapabilityName::new(capability).map_err(|error| {
                CaapError::compiler(format!("bootstrap capability name is invalid: {error}"))
            })?);
        }
        let Some(existing) = self.grants.get(unit_id) else {
            return Ok(false);
        };
        let changed = normalized
            .iter()
            .any(|capability| existing.contains(capability));
        if !changed {
            return Ok(false);
        }
        let version = self.next_version()?;
        let Some(existing) = self.grants.get_mut(unit_id) else {
            return Ok(false);
        };
        for capability in normalized {
            existing.remove(capability.as_str());
        }
        if existing.is_empty() {
            self.grants.remove(unit_id);
        }
        self.version = version;
        Ok(true)
    }

    pub fn allows(&self, unit_id: &str, capability: &str) -> bool {
        let Ok(capability) = CapabilityName::new(capability) else {
            return false;
        };
        self.grants
            .get(unit_id)
            .is_some_and(|capabilities| capabilities.iter().any(|grant| grant.covers(&capability)))
    }

    pub fn require(&self, unit_id: &str, capability: &str) -> CaapResult<()> {
        if unit_id.is_empty() {
            return Err(CaapError::compiler(
                "bootstrap capability unit id must be non-empty",
            ));
        }
        validate_bootstrap_capability_name(capability)?;
        if self.allows(unit_id, capability) {
            Ok(())
        } else {
            Err(CaapError::compiler(format!(
                "bootstrap capability denied for {unit_id}: {capability}"
            )))
        }
    }

    pub fn capabilities_for(&self, unit_id: &str) -> Vec<CapabilityName> {
        self.grants
            .get(unit_id)
            .map(|capabilities| capabilities.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn unit_ids(&self) -> Vec<&str> {
        self.grants.keys().map(String::as_str).collect()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    fn next_version(&self) -> CaapResult<u64> {
        self.version
            .checked_add(1)
            .ok_or_else(|| CaapError::compiler("bootstrap capability graph version overflow"))
    }
}

impl<'de> Deserialize<'de> for BootstrapCapabilityGraph {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct BootstrapCapabilityGraphData {
            grants: BTreeMap<String, BTreeSet<CapabilityName>>,
            version: u64,
        }

        let data = BootstrapCapabilityGraphData::deserialize(deserializer)?;
        for (unit_id, capabilities) in &data.grants {
            if unit_id.is_empty() {
                return Err(serde::de::Error::custom(
                    "bootstrap capability unit id must be non-empty",
                ));
            }
            if capabilities.is_empty() {
                return Err(serde::de::Error::custom(format!(
                    "bootstrap capability grants for {unit_id} must be non-empty"
                )));
            }
        }
        Ok(Self {
            grants: data.grants,
            version: data.version,
        })
    }
}

impl BootstrapImage {
    pub fn unit_ids(&self) -> Vec<&str> {
        self.units
            .iter()
            .map(|unit| unit.unit_id.as_str())
            .collect()
    }

    pub fn validate(&self) -> CaapResult<()> {
        if self.name.is_empty() {
            return Err(CaapError::compiler(
                "bootstrap image name must be non-empty",
            ));
        }
        let mut unit_ids = BTreeSet::new();
        for unit in &self.units {
            unit.validate().map_err(|err| {
                CaapError::compiler(format!("bootstrap image unit template is invalid: {err}"))
            })?;
            if !unit_ids.insert(unit.unit_id.clone()) {
                return Err(CaapError::compiler(format!(
                    "bootstrap image contains duplicate unit id: {}",
                    unit.unit_id
                )));
            }
        }
        for unit_id in self.capabilities.unit_ids() {
            if !unit_ids.contains(unit_id) {
                return Err(CaapError::compiler(format!(
                    "bootstrap image capability grant references missing unit: {unit_id}"
                )));
            }
        }
        self.fact_schema.validate()?;
        let mut base_semantic_entries = BTreeSet::new();
        for entry in &self.base_semantic_entries {
            validate_base_semantic_entry(entry)?;
            if !base_semantic_entries.insert(entry.name.clone()) {
                return Err(CaapError::compiler(format!(
                    "bootstrap image contains duplicate base semantic entry: {}",
                    entry.name
                )));
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for BootstrapImage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct BootstrapImageData {
            name: String,
            units: Vec<UnitTemplate>,
            capabilities: BootstrapCapabilityGraph,
            #[serde(default)]
            fact_schema: FactSchemaRegistry,
            #[serde(default)]
            base_semantic_entries: Vec<SemanticEntry>,
            session_version: u64,
        }

        let data = BootstrapImageData::deserialize(deserializer)?;
        let image = Self {
            name: data.name,
            units: data.units,
            capabilities: data.capabilities,
            fact_schema: data.fact_schema,
            base_semantic_entries: data.base_semantic_entries,
            session_version: data.session_version,
        };
        image
            .validate()
            .map_err(serde::de::Error::custom)
            .map(|()| image)
    }
}

impl BootstrapImageFile {
    pub const FORMAT_NAME: &'static str = "caap_bootstrap_image";
    pub const FORMAT_VERSION: u32 = 1;

    pub fn new(image: BootstrapImage) -> Self {
        Self {
            format_name: Self::FORMAT_NAME.to_string(),
            format_version: Self::FORMAT_VERSION,
            image,
        }
    }

    pub fn validate(&self) -> CaapResult<()> {
        if self.format_name != Self::FORMAT_NAME {
            return Err(CaapError::compiler(
                "bootstrap image file format name is unsupported",
            ));
        }
        if self.format_version != Self::FORMAT_VERSION {
            return Err(CaapError::compiler(
                "bootstrap image file format version is unsupported",
            ));
        }
        self.image.validate()?;
        Ok(())
    }

    pub fn to_json_string(&self) -> CaapResult<String> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(|error| {
            CaapError::compiler(format!("failed to serialize bootstrap image file: {error}"))
        })
    }

    pub fn from_json_str(text: &str) -> CaapResult<Self> {
        let image_file: Self = serde_json::from_str(text).map_err(|error| {
            CaapError::compiler(format!(
                "failed to deserialize bootstrap image file: {error}"
            ))
        })?;
        image_file.validate()?;
        Ok(image_file)
    }

    pub fn write_json_file(&self, path: impl AsRef<Path>) -> CaapResult<()> {
        let path = path.as_ref();
        fs::write(path, self.to_json_string()?).map_err(|error| {
            CaapError::compiler(format!(
                "failed to write bootstrap image file {}: {error}",
                path.display()
            ))
        })
    }

    pub fn read_json_file(path: impl AsRef<Path>) -> CaapResult<Self> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|error| {
            CaapError::compiler(format!(
                "failed to read bootstrap image file {}: {error}",
                path.display()
            ))
        })?;
        Self::from_json_str(&text)
    }
}

impl<'de> Deserialize<'de> for BootstrapImageFile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct BootstrapImageFileData {
            format_name: String,
            format_version: u32,
            image: BootstrapImage,
        }

        let data = BootstrapImageFileData::deserialize(deserializer)?;
        let image_file = Self {
            format_name: data.format_name,
            format_version: data.format_version,
            image: data.image,
        };
        image_file
            .validate()
            .map_err(serde::de::Error::custom)
            .map(|()| image_file)
    }
}

impl BootstrapImageTrustPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_trusted_fingerprint(mut self, fingerprint: impl Into<String>) -> CaapResult<Self> {
        self.trust_fingerprint(fingerprint)?;
        Ok(self)
    }

    pub fn trust_fingerprint(&mut self, fingerprint: impl Into<String>) -> CaapResult<()> {
        let fingerprint = fingerprint.into();
        validate_bootstrap_image_fingerprint(&fingerprint)?;
        self.trusted_fingerprints.insert(fingerprint);
        Ok(())
    }

    pub fn replace_trusted_fingerprints(
        &mut self,
        fingerprints: impl IntoIterator<Item = String>,
    ) -> CaapResult<()> {
        let mut trusted_fingerprints = BTreeSet::new();
        for fingerprint in fingerprints {
            validate_bootstrap_image_fingerprint(&fingerprint)?;
            trusted_fingerprints.insert(fingerprint);
        }
        self.trusted_fingerprints = trusted_fingerprints;
        Ok(())
    }

    pub fn trust_file(&mut self, path: impl AsRef<Path>) -> CaapResult<String> {
        let fingerprint = bootstrap_image_file_fingerprint(path)?;
        self.trust_fingerprint(fingerprint.clone())?;
        Ok(fingerprint)
    }

    pub fn revoke_fingerprint(&mut self, fingerprint: &str) -> CaapResult<bool> {
        validate_bootstrap_image_fingerprint(fingerprint)?;
        Ok(self.trusted_fingerprints.remove(fingerprint))
    }

    pub fn clear(&mut self) {
        self.trusted_fingerprints.clear();
    }

    pub fn is_trusted_fingerprint(&self, fingerprint: &str) -> bool {
        self.trusted_fingerprints.contains(fingerprint)
    }

    pub fn require_fingerprint(&self, fingerprint: &str) -> CaapResult<()> {
        if self.is_trusted_fingerprint(fingerprint) {
            Ok(())
        } else {
            Err(CaapError::compiler(format!(
                "bootstrap image file fingerprint is not trusted: {fingerprint}"
            )))
        }
    }

    pub fn trusted_fingerprints(&self) -> Vec<&str> {
        self.trusted_fingerprints
            .iter()
            .map(String::as_str)
            .collect()
    }
}

fn validate_bootstrap_image_fingerprint(fingerprint: &str) -> CaapResult<()> {
    if fingerprint.is_empty() {
        return Err(CaapError::compiler(
            "bootstrap image trusted fingerprint must be non-empty",
        ));
    }
    if fingerprint.trim() != fingerprint || fingerprint.chars().any(char::is_whitespace) {
        return Err(CaapError::compiler(
            "bootstrap image trusted fingerprint must not contain whitespace",
        ));
    }
    Ok(())
}

impl BootstrapImageStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn store(&mut self, image: BootstrapImage) -> CaapResult<()> {
        image.validate()?;
        let version = self.next_version()?;
        self.images.insert(image.name.clone(), image);
        self.version = version;
        Ok(())
    }

    pub fn get(&self, name: &str) -> CaapResult<Option<&BootstrapImage>> {
        if name.is_empty() {
            return Err(CaapError::compiler(
                "bootstrap image name must be non-empty",
            ));
        }
        Ok(self.images.get(name))
    }

    pub fn image_file(&self, name: &str) -> CaapResult<BootstrapImageFile> {
        let image = self.get(name)?.cloned().ok_or_else(|| {
            CaapError::compiler(format!("bootstrap image does not exist: {name}"))
        })?;
        Ok(BootstrapImageFile::new(image))
    }

    pub fn restore_image_file(&mut self, image_file: BootstrapImageFile) -> CaapResult<()> {
        image_file.validate()?;
        self.store(image_file.image)
    }

    pub fn save_image_file(&self, name: &str, path: impl AsRef<Path>) -> CaapResult<()> {
        self.image_file(name)?.write_json_file(path)
    }

    pub fn load_image_file(&mut self, path: impl AsRef<Path>) -> CaapResult<()> {
        self.restore_image_file(BootstrapImageFile::read_json_file(path)?)
    }

    pub fn image_names(&self) -> Vec<&str> {
        self.images.keys().map(String::as_str).collect()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    fn next_version(&self) -> CaapResult<u64> {
        self.version
            .checked_add(1)
            .ok_or_else(|| CaapError::compiler("bootstrap image store version overflow"))
    }
}

impl<'a> CompilerBootstrapController<'a> {
    pub fn grant_capability(
        &mut self,
        unit_id: impl Into<String>,
        capability: impl Into<String>,
    ) -> CaapResult<()> {
        let changed = self
            .compiler
            .bootstrap
            .capabilities
            .grant(unit_id, capability)?;
        if changed {
            self.compiler.advance_session_version()?;
        }
        Ok(())
    }

    pub fn grant_capabilities(
        &mut self,
        unit_id: impl Into<String>,
        capabilities: impl IntoIterator<Item = String>,
    ) -> CaapResult<()> {
        let changed = self
            .compiler
            .bootstrap
            .capabilities
            .grant_many(unit_id, capabilities)?;
        if changed {
            self.compiler.advance_session_version()?;
        }
        Ok(())
    }

    pub fn revoke_capability(&mut self, unit_id: &str, capability: &str) -> CaapResult<()> {
        if self
            .compiler
            .bootstrap
            .capabilities
            .revoke(unit_id, capability)?
        {
            self.compiler.advance_session_version()?;
        }
        Ok(())
    }

    pub fn require_capability(&self, unit_id: &str, capability: &str) -> CaapResult<()> {
        self.compiler
            .bootstrap
            .capabilities
            .require(unit_id, capability)
    }

    pub fn execute_text_with_capabilities(
        &mut self,
        text: impl Into<String>,
        unit_id: impl Into<String>,
        capabilities: impl IntoIterator<Item = String>,
    ) -> EvalResult {
        let unit_id = unit_id.into();
        self.grant_capabilities(unit_id.clone(), capabilities)
            .map_err(EvalSignal::from)?;
        self.execute_text(text, unit_id)
    }

    /// Like [`Self::execute_text_with_capabilities`] but pushes the capabilities
    /// onto the *ambient* capability stack for the duration of the run, instead
    /// of granting them to `unit_id` alone. Functions the text calls — defined in
    /// other, ungranted units (e.g. the loader's `surface_of`, which reads a file
    /// header through the `fs` host service) — only see a capability via the
    /// stack; a per-unit grant covers code whose *current* bootstrap unit is
    /// `unit_id`, which a cross-unit call is not. This mirrors how
    /// `ctfe_compiler_evaluate_bootstrap_file` grants a program it runs.
    pub fn execute_text_with_capability_scope(
        &mut self,
        text: impl Into<String>,
        unit_id: impl Into<String>,
        capabilities: impl IntoIterator<Item = String>,
    ) -> EvalResult {
        let capabilities =
            normalize_bootstrap_capabilities(capabilities).map_err(EvalSignal::from)?;
        self.compiler.bootstrap.capability_stack.push(capabilities);
        let result = self.execute_text(text, unit_id);
        self.compiler.bootstrap.capability_stack.pop();
        result
    }

    pub fn execute_virtual_file(
        &mut self,
        vfs: &BootstrapVirtualFileSystem,
        path: impl Into<String>,
        unit_id: impl Into<String>,
    ) -> EvalResult {
        let path = path.into();
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err(EvalSignal::from(CaapError::compiler(
                "bootstrap unit id must be non-empty",
            )));
        }
        let path = normalize_virtual_path(path).map_err(EvalSignal::from)?;
        let depth = self
            .compiler
            .bootstrap
            .enter_execution()
            .map_err(EvalSignal::from)?;
        let started = Instant::now();
        let result = self.execute_virtual_file_inner(vfs, path.clone(), unit_id, depth);
        let elapsed_ms = elapsed_ms_string(started);
        self.compiler
            .bootstrap
            .leave_execution()
            .map_err(EvalSignal::from)?;
        let action = if depth == 0 {
            "bootstrap.vfs"
        } else {
            "bootstrap.nested_vfs"
        };
        let target = format!("vfs:{path}");
        let trace_result =
            self.compiler
                .push_bootstrap_trace(action, target.clone(), depth, result.is_ok());
        let event_result = self.compiler.emit_compiler_event(
            "bootstrap.execute",
            Some(target),
            "executed bootstrap source",
            [
                ("action".to_string(), action.to_string()),
                ("depth".to_string(), depth.to_string()),
                ("elapsed_ms".to_string(), elapsed_ms),
                ("succeeded".to_string(), result.is_ok().to_string()),
            ],
        );
        match (result, trace_result, event_result) {
            (Ok(_), Err(error), _) | (Ok(_), _, Err(error)) => Err(EvalSignal::from(error)),
            (result, _, _) => result,
        }
    }

    pub fn execute_file(
        &mut self,
        path: impl AsRef<Path>,
        unit_id: impl Into<String>,
    ) -> EvalResult {
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err(EvalSignal::from(CaapError::compiler(
                "bootstrap unit id must be non-empty",
            )));
        }
        let resolved = resolve_source_path(path.as_ref()).map_err(EvalSignal::from)?;
        let target = path_to_string(&resolved).map_err(EvalSignal::from)?;
        let depth = self
            .compiler
            .bootstrap
            .enter_execution()
            .map_err(EvalSignal::from)?;
        let started = Instant::now();
        let result = self.execute_file_inner(resolved, unit_id, depth);
        let elapsed_ms = elapsed_ms_string(started);
        self.compiler
            .bootstrap
            .leave_execution()
            .map_err(EvalSignal::from)?;
        let action = if depth == 0 {
            "bootstrap.raw"
        } else {
            "bootstrap.nested_raw"
        };
        let trace_result =
            self.compiler
                .push_bootstrap_trace(action, target.clone(), depth, result.is_ok());
        let event_result = self.compiler.emit_compiler_event(
            "bootstrap.execute",
            Some(target),
            "executed bootstrap source",
            [
                ("action".to_string(), action.to_string()),
                ("depth".to_string(), depth.to_string()),
                ("elapsed_ms".to_string(), elapsed_ms),
                ("succeeded".to_string(), result.is_ok().to_string()),
            ],
        );
        match (result, trace_result, event_result) {
            (Ok(_), Err(error), _) | (Ok(_), _, Err(error)) => Err(EvalSignal::from(error)),
            (result, _, _) => result,
        }
    }

    pub fn execute_text(
        &mut self,
        text: impl Into<String>,
        unit_id: impl Into<String>,
    ) -> EvalResult {
        let text = text.into();
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err(EvalSignal::from(CaapError::compiler(
                "bootstrap unit id must be non-empty",
            )));
        }
        if text.is_empty() {
            return Err(EvalSignal::from(CaapError::compiler(
                "bootstrap text must be non-empty",
            )));
        }

        let depth = self
            .compiler
            .bootstrap
            .enter_execution()
            .map_err(EvalSignal::from)?;
        let started = Instant::now();
        let result = self.execute_text_inner(text, unit_id.clone(), depth);
        let elapsed_ms = elapsed_ms_string(started);
        self.compiler
            .bootstrap
            .leave_execution()
            .map_err(EvalSignal::from)?;

        let action = if depth == 0 {
            "bootstrap.raw"
        } else {
            "bootstrap.nested_raw"
        };
        let trace_result =
            self.compiler
                .push_bootstrap_trace(action, unit_id.clone(), depth, result.is_ok());
        let event_result = self.compiler.emit_compiler_event(
            "bootstrap.execute",
            Some(unit_id),
            "executed bootstrap source",
            [
                ("action".to_string(), action.to_string()),
                ("depth".to_string(), depth.to_string()),
                ("elapsed_ms".to_string(), elapsed_ms),
                ("succeeded".to_string(), result.is_ok().to_string()),
            ],
        );
        match (result, trace_result, event_result) {
            (Ok(_), Err(error), _) | (Ok(_), _, Err(error)) => Err(EvalSignal::from(error)),
            (result, _, _) => result,
        }
    }

    fn execute_text_inner(&mut self, text: String, unit_id: String, _depth: usize) -> EvalResult {
        self.compiler
            .record_bootstrap_execution(format!("<inline:{unit_id}>"))
            .map_err(EvalSignal::from)?;
        let template = self
            .compiler
            .load_surface_text_template(text, unit_id.clone())
            .map_err(EvalSignal::from)?
            .template;
        let unit = Unit::from_template(template).map_err(EvalSignal::from)?;
        self.compiler
            .register_unit(unit.clone())
            .map_err(EvalSignal::from)?;
        self.compiler.evaluation().evaluate(
            &unit,
            PhasePolicy::CompileTime,
            Vec::<(String, RuntimeValue)>::new(),
        )
    }

    fn execute_file_inner(&mut self, path: PathBuf, unit_id: String, _depth: usize) -> EvalResult {
        let path_string = path_to_string(&path).map_err(EvalSignal::from)?;
        self.compiler
            .record_bootstrap_execution(path_string)
            .map_err(EvalSignal::from)?;
        let template = self
            .compiler
            .load_surface_path_template(path, unit_id)
            .map_err(EvalSignal::from)?
            .template;
        let unit = Unit::from_template(template).map_err(EvalSignal::from)?;
        self.compiler
            .register_unit(unit.clone())
            .map_err(EvalSignal::from)?;
        self.compiler.evaluation().evaluate(
            &unit,
            PhasePolicy::CompileTime,
            Vec::<(String, RuntimeValue)>::new(),
        )
    }

    fn execute_virtual_file_inner(
        &mut self,
        vfs: &BootstrapVirtualFileSystem,
        path: String,
        unit_id: String,
        _depth: usize,
    ) -> EvalResult {
        let text = vfs.read(&path).map_err(EvalSignal::from)?;
        if text.is_empty() {
            return Err(EvalSignal::from(CaapError::compiler(
                "bootstrap virtual file text must be non-empty",
            )));
        }
        self.compiler
            .record_bootstrap_execution(format!("<vfs:{path}>"))
            .map_err(EvalSignal::from)?;
        let template = self
            .compiler
            .load_surface_virtual_template(path, text.to_string(), unit_id)
            .map_err(EvalSignal::from)?
            .template;
        let unit = Unit::from_template(template).map_err(EvalSignal::from)?;
        self.compiler
            .register_unit(unit.clone())
            .map_err(EvalSignal::from)?;
        self.compiler.evaluation().evaluate(
            &unit,
            PhasePolicy::CompileTime,
            Vec::<(String, RuntimeValue)>::new(),
        )
    }
}

#[cfg(test)]
mod capability_vocabulary_tests {
    use super::{
        normalize_bootstrap_capabilities, BootstrapCapabilityGraph, BootstrapImage,
        BootstrapImageStore, BootstrapVirtualFileSystem,
    };
    use crate::compiler::FactSchemaRegistry;
    use crate::unit::Unit;

    #[test]
    fn normalize_accepts_explicit_capabilities() {
        assert!(normalize_bootstrap_capabilities(["sys".to_string()]).is_ok());
        assert!(normalize_bootstrap_capabilities(["sys.fs.read".to_string()]).is_ok());
        assert!(normalize_bootstrap_capabilities(["test.host".to_string()]).is_ok());
    }

    #[test]
    fn normalize_rejects_host_services_alias() {
        let err = normalize_bootstrap_capabilities(["host_services".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("unsupported bootstrap internal capability"));
    }

    #[test]
    fn fine_grained_grants_match_hierarchically() {
        let mut graph = BootstrapCapabilityGraph::new();
        graph.grant("reader", "sys.fs.read").unwrap();
        graph.grant("fs", "sys.fs").unwrap();
        assert!(graph.allows("reader", "sys.fs.read"));
        assert!(!graph.allows("reader", "sys.fs.write"));
        assert!(graph.allows("fs", "sys.fs.read"));
        assert!(graph.allows("fs", "sys.fs.write"));
        assert!(!graph.allows("fs", "sys.net"));
    }

    #[test]
    fn virtual_file_insert_rejects_version_overflow_without_mutating() {
        let mut vfs = BootstrapVirtualFileSystem {
            version: u64::MAX,
            ..BootstrapVirtualFileSystem::new()
        };

        let error = vfs.insert("module.caap", "text").unwrap_err().to_string();

        assert!(error.contains("bootstrap virtual file system version overflow"));
        assert!(!vfs.contains("module.caap"));
        assert_eq!(vfs.version(), u64::MAX);
    }

    #[test]
    fn capability_grant_many_rejects_invalid_capability_without_partial_mutation() {
        let mut graph = BootstrapCapabilityGraph::new();

        let error = graph
            .grant_many(
                "module",
                ["sys.fs.read".to_string(), "bad capability".to_string()],
            )
            .unwrap_err()
            .to_string();

        assert!(error.contains("bootstrap capability name is invalid"));
        assert!(graph.capabilities_for("module").is_empty());
        assert_eq!(graph.version(), 0);
    }

    #[test]
    fn capability_grant_rejects_version_overflow_without_mutating() {
        let mut graph = BootstrapCapabilityGraph {
            version: u64::MAX,
            ..BootstrapCapabilityGraph::new()
        };

        let error = graph
            .grant("module", "sys.fs.read")
            .unwrap_err()
            .to_string();

        assert!(error.contains("bootstrap capability graph version overflow"));
        assert!(graph.capabilities_for("module").is_empty());
        assert_eq!(graph.version(), u64::MAX);
    }

    #[test]
    fn capability_revoke_many_rejects_invalid_capability_without_partial_mutation() {
        let mut graph = BootstrapCapabilityGraph::new();
        graph
            .grant_many(
                "module",
                ["sys.fs.read".to_string(), "sys.fs.write".to_string()],
            )
            .unwrap();
        let version = graph.version();

        let error = graph
            .revoke_many("module", ["sys.fs.read", "bad capability"])
            .unwrap_err()
            .to_string();

        assert!(error.contains("bootstrap capability name is invalid"));
        assert!(graph.allows("module", "sys.fs.read"));
        assert!(graph.allows("module", "sys.fs.write"));
        assert_eq!(graph.version(), version);
    }

    #[test]
    fn image_store_rejects_version_overflow_without_mutating() {
        let unit = Unit::empty("bootstrap.unit").unwrap().to_template();
        let image = BootstrapImage {
            name: "base".to_string(),
            units: vec![unit],
            capabilities: BootstrapCapabilityGraph::new(),
            fact_schema: FactSchemaRegistry::new(),
            base_semantic_entries: Vec::new(),
            session_version: 0,
        };
        let mut store = BootstrapImageStore {
            version: u64::MAX,
            ..BootstrapImageStore::new()
        };

        let error = store.store(image).unwrap_err().to_string();

        assert!(error.contains("bootstrap image store version overflow"));
        assert!(store.get("base").unwrap().is_none());
        assert_eq!(store.version(), u64::MAX);
    }
}
