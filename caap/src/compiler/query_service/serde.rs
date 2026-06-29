//! Semantic value serialization and deserialization for cached query stage artifacts
//! and provider execution records.
use std::collections::BTreeSet;

use crate::artifacts::{ArtifactKey, ArtifactValue};
use crate::error::{CaapError, CaapResult};
use crate::semantic::{PhasePolicy, SemanticValue};
use crate::unit::UnitTemplate;

use super::super::query_provider::{
    normalize_cache_scope, normalize_resume_policy, EffectSet, QueryProviderExecutionRecord,
};
use super::cache::collect_record_strings;

const QUERY_STAGE_SUMMARY_FIELDS: &[&str] = &[
    "stage",
    "unit",
    "phase",
    "unit_version",
    "cache_written_at_unix_ns",
    "providers",
    "provider_count",
    "effect_tags",
    "reads_subjects",
    "writes_subjects",
    "read_cells",
    "write_cells",
    "reads_files",
    "writes_files",
    "restarted",
    "restart_target",
    "artifact_key",
    "execution_summary",
];

const QUERY_PROVIDER_EXECUTION_RECORD_FIELDS: &[&str] = &[
    "recorded_at_unix_ns",
    "provider_name",
    "stage",
    "family",
    "phase_policy",
    "effect_tags",
    "requires",
    "requires_data",
    "provides_data",
    "provides",
    "reads",
    "writes",
    "artifact_dependencies",
    "reads_subjects",
    "writes_subjects",
    "read_cells",
    "write_cells",
    "reads_files",
    "writes_files",
    "cache_scope",
    "resume_policy",
    "iteration",
    "changed",
    "diagnostics_emitted",
    "rolled_back",
    "stopped_by_error",
    "outcome_kind",
    "diagnostic_codes",
    "rewrite_count",
    "erased_count",
    "touched_node_kinds",
    "change_domains",
    "restart_requested",
    "restart_stage",
    "outcome_summary",
];

#[cfg(test)]
pub fn cached_execution_records(
    value: &ArtifactValue,
) -> CaapResult<Vec<QueryProviderExecutionRecord>> {
    decode_cached_execution_records(value).map(|(_, records)| records)
}

pub struct CachedQueryStageReplayRequest<'a> {
    pub stage: &'a str,
    pub phase: PhasePolicy,
    pub unit_id: &'a str,
}

type CachedExecutionRecordDecode<'a> = (
    &'a [(String, SemanticValue)],
    Vec<QueryProviderExecutionRecord>,
);

pub fn cached_execution_records_for_request(
    value: &ArtifactValue,
    request: CachedQueryStageReplayRequest<'_>,
) -> CaapResult<Vec<QueryProviderExecutionRecord>> {
    let (entries, records) = decode_cached_execution_records(value)?;
    validate_query_stage_summary_request(entries, request)?;
    Ok(records)
}

fn decode_cached_execution_records(
    value: &ArtifactValue,
) -> CaapResult<CachedExecutionRecordDecode<'_>> {
    let entries = query_stage_summary_entries(value)?;
    reject_unknown_or_duplicate_query_stage_summary_fields(entries)?;
    let Some(SemanticValue::List(records)) = semantic_map_get(entries, "execution_summary") else {
        return Err(CaapError::compiler(
            "cached query stage artifact has missing or malformed execution_summary",
        ));
    };
    if !matches!(
        semantic_map_get(entries, "cache_written_at_unix_ns"),
        Some(SemanticValue::Int(value)) if *value > 0
    ) {
        return Err(CaapError::compiler(
            "cached query stage artifact has missing or malformed cache_written_at_unix_ns",
        ));
    }
    validate_query_stage_summary_fields(entries)?;
    let records = records
        .iter()
        .enumerate()
        .map(|(index, value)| query_provider_execution_record_from_semantic_value(value, index))
        .collect::<CaapResult<Vec<_>>>()?;
    validate_query_stage_summary_record_consistency(entries, &records)?;
    Ok((entries, records))
}

pub fn cached_query_stage_unit_template(value: &ArtifactValue) -> CaapResult<Option<UnitTemplate>> {
    let ArtifactValue::QueryStage(cached) = value else {
        return Ok(None);
    };
    cached.unit_template.validate().map_err(|error| {
        CaapError::compiler(format!(
            "cached query stage artifact unit template is invalid: {error}"
        ))
    })?;
    Ok(Some(cached.unit_template.clone()))
}

fn query_stage_summary_entries(value: &ArtifactValue) -> CaapResult<&[(String, SemanticValue)]> {
    match value {
        ArtifactValue::Semantic(SemanticValue::Map(entries)) => Ok(entries),
        ArtifactValue::QueryStage(cached) => {
            let SemanticValue::Map(entries) = &cached.summary else {
                return Err(CaapError::compiler(
                    "cached query stage artifact summary must be a semantic map",
                ));
            };
            Ok(entries)
        }
        _ => Err(CaapError::compiler(
            "cached query stage artifact must be a semantic map",
        )),
    }
}

pub fn provider_execution_record_to_semantic_value(
    record: &QueryProviderExecutionRecord,
) -> CaapResult<SemanticValue> {
    SemanticValue::map([
        (
            "recorded_at_unix_ns".to_string(),
            SemanticValue::Int(record.recorded_at_unix_ns),
        ),
        (
            "provider_name".to_string(),
            SemanticValue::Str(record.provider_name.clone()),
        ),
        (
            "stage".to_string(),
            SemanticValue::Str(record.stage.clone()),
        ),
        (
            "family".to_string(),
            optional_semantic_string(record.family.as_deref()),
        ),
        (
            "phase_policy".to_string(),
            SemanticValue::Str(record.phase_policy.as_str().to_string()),
        ),
        (
            "effect_tags".to_string(),
            semantic_effect_set(&record.effect_tags),
        ),
        (
            "requires".to_string(),
            semantic_string_list(&record.requires),
        ),
        (
            "requires_data".to_string(),
            semantic_string_list(&record.requires_data),
        ),
        (
            "provides_data".to_string(),
            semantic_string_list(&record.provides_data),
        ),
        (
            "provides".to_string(),
            semantic_string_list(&record.provides),
        ),
        ("reads".to_string(), semantic_string_list(&record.reads)),
        ("writes".to_string(), semantic_string_list(&record.writes)),
        (
            "artifact_dependencies".to_string(),
            semantic_artifact_key_list(&record.artifact_dependencies),
        ),
        (
            "reads_subjects".to_string(),
            semantic_string_list(&record.reads_subjects),
        ),
        (
            "writes_subjects".to_string(),
            semantic_string_list(&record.writes_subjects),
        ),
        (
            "read_cells".to_string(),
            semantic_string_list(&record.read_cells),
        ),
        (
            "write_cells".to_string(),
            semantic_string_list(&record.write_cells),
        ),
        (
            "reads_files".to_string(),
            semantic_string_list(&record.reads_files),
        ),
        (
            "writes_files".to_string(),
            semantic_string_list(&record.writes_files),
        ),
        (
            "cache_scope".to_string(),
            SemanticValue::Str(record.cache_scope.as_str().to_string()),
        ),
        (
            "resume_policy".to_string(),
            SemanticValue::Str(record.resume_policy.as_str().to_string()),
        ),
        (
            "iteration".to_string(),
            SemanticValue::Int(record.iteration as i64),
        ),
        ("changed".to_string(), SemanticValue::Bool(record.changed)),
        (
            "diagnostics_emitted".to_string(),
            SemanticValue::Int(record.diagnostics_emitted as i64),
        ),
        (
            "rolled_back".to_string(),
            SemanticValue::Bool(record.rolled_back),
        ),
        (
            "stopped_by_error".to_string(),
            SemanticValue::Bool(record.stopped_by_error),
        ),
        (
            "outcome_kind".to_string(),
            SemanticValue::Str(record.outcome_kind.clone()),
        ),
        (
            "diagnostic_codes".to_string(),
            semantic_string_list(&record.diagnostic_codes),
        ),
        (
            "rewrite_count".to_string(),
            SemanticValue::Int(record.rewrite_count as i64),
        ),
        (
            "erased_count".to_string(),
            SemanticValue::Int(record.erased_count as i64),
        ),
        (
            "touched_node_kinds".to_string(),
            semantic_string_list(&record.touched_node_kinds),
        ),
        (
            "change_domains".to_string(),
            semantic_string_list(&record.change_domains),
        ),
        (
            "restart_requested".to_string(),
            SemanticValue::Bool(record.restart_requested),
        ),
        (
            "restart_stage".to_string(),
            optional_semantic_string(record.restart_stage.as_deref()),
        ),
        (
            "outcome_summary".to_string(),
            SemanticValue::map(
                record
                    .outcome_summary
                    .iter()
                    .map(|(key, value)| (key.clone(), SemanticValue::Str(value.clone()))),
            )?,
        ),
    ])
}

fn query_provider_execution_record_from_semantic_value(
    value: &SemanticValue,
    index: usize,
) -> CaapResult<QueryProviderExecutionRecord> {
    let SemanticValue::Map(entries) = value else {
        return Err(cached_record_error(index, "record", "semantic map"));
    };
    reject_unknown_or_duplicate_semantic_fields(
        entries,
        QUERY_PROVIDER_EXECUTION_RECORD_FIELDS,
        index,
    )?;
    let cache_scope = required_semantic_field(entries, "cache_scope", index, semantic_string)
        .and_then(|value| {
            normalize_cache_scope(value)
                .map_err(|_| cached_record_error(index, "cache_scope", "one of: none, unit"))
        })?;
    let resume_policy = required_semantic_field(entries, "resume_policy", index, semantic_string)
        .and_then(|value| {
        normalize_resume_policy(value).map_err(|_| {
            cached_record_error(
                index,
                "resume_policy",
                "one of: safe, never, bootstrap_safe",
            )
        })
    })?;

    Ok(QueryProviderExecutionRecord {
        recorded_at_unix_ns: required_semantic_field(
            entries,
            "recorded_at_unix_ns",
            index,
            semantic_positive_i64,
        )?,
        provider_name: required_semantic_field(entries, "provider_name", index, semantic_string)?,
        stage: required_semantic_field(entries, "stage", index, semantic_string)?,
        family: required_semantic_field(entries, "family", index, semantic_optional_string)?,
        phase_policy: required_semantic_field(
            entries,
            "phase_policy",
            index,
            semantic_phase_policy,
        )?,
        effect_tags: EffectSet::from_unique_strings(
            required_semantic_field(entries, "effect_tags", index, semantic_unique_string_list)?,
            "cached query execution record effect tag",
        )?,
        requires: required_semantic_field(entries, "requires", index, semantic_unique_string_list)?,
        requires_data: required_semantic_field(
            entries,
            "requires_data",
            index,
            semantic_unique_string_list,
        )?,
        provides_data: required_semantic_field(
            entries,
            "provides_data",
            index,
            semantic_unique_string_list,
        )?,
        provides: required_semantic_field(entries, "provides", index, semantic_unique_string_list)?,
        reads: required_semantic_field(entries, "reads", index, semantic_unique_string_list)?,
        writes: required_semantic_field(entries, "writes", index, semantic_unique_string_list)?,
        artifact_dependencies: required_semantic_field(
            entries,
            "artifact_dependencies",
            index,
            semantic_unique_artifact_key_list,
        )?,
        reads_subjects: required_semantic_field(
            entries,
            "reads_subjects",
            index,
            semantic_unique_string_list,
        )?,
        writes_subjects: required_semantic_field(
            entries,
            "writes_subjects",
            index,
            semantic_unique_string_list,
        )?,
        read_cells: required_semantic_field(
            entries,
            "read_cells",
            index,
            semantic_unique_string_list,
        )?,
        write_cells: required_semantic_field(
            entries,
            "write_cells",
            index,
            semantic_unique_string_list,
        )?,
        reads_files: required_semantic_field(
            entries,
            "reads_files",
            index,
            semantic_unique_string_list,
        )?,
        writes_files: required_semantic_field(
            entries,
            "writes_files",
            index,
            semantic_unique_string_list,
        )?,
        cache_scope,
        resume_policy,
        iteration: required_semantic_field(entries, "iteration", index, semantic_usize)?,
        changed: required_semantic_field(entries, "changed", index, semantic_bool)?,
        diagnostics_emitted: required_semantic_field(
            entries,
            "diagnostics_emitted",
            index,
            semantic_usize,
        )?,
        rolled_back: required_semantic_field(entries, "rolled_back", index, semantic_bool)?,
        stopped_by_error: required_semantic_field(
            entries,
            "stopped_by_error",
            index,
            semantic_bool,
        )?,
        outcome_kind: required_semantic_field(entries, "outcome_kind", index, semantic_string)?,
        diagnostic_codes: required_semantic_field(
            entries,
            "diagnostic_codes",
            index,
            semantic_string_list_field,
        )?,
        rewrite_count: required_semantic_field(entries, "rewrite_count", index, semantic_usize)?,
        erased_count: required_semantic_field(entries, "erased_count", index, semantic_usize)?,
        touched_node_kinds: required_semantic_field(
            entries,
            "touched_node_kinds",
            index,
            semantic_unique_string_list,
        )?,
        change_domains: required_semantic_field(
            entries,
            "change_domains",
            index,
            semantic_unique_string_list,
        )?,
        restart_requested: required_semantic_field(
            entries,
            "restart_requested",
            index,
            semantic_bool,
        )?,
        restart_stage: required_semantic_field(
            entries,
            "restart_stage",
            index,
            semantic_optional_string,
        )?,
        outcome_summary: required_semantic_field(
            entries,
            "outcome_summary",
            index,
            semantic_string_pairs,
        )?,
    })
}

fn reject_unknown_or_duplicate_query_stage_summary_fields(
    entries: &[(String, SemanticValue)],
) -> CaapResult<()> {
    let mut seen = BTreeSet::new();
    for (key, _) in entries {
        if !QUERY_STAGE_SUMMARY_FIELDS.contains(&key.as_str()) {
            return Err(CaapError::compiler(format!(
                "cached query stage artifact summary contains unknown field '{key}'"
            )));
        }
        if !seen.insert(key.as_str()) {
            return Err(CaapError::compiler(format!(
                "cached query stage artifact summary field '{key}' must be present exactly once"
            )));
        }
    }
    Ok(())
}

fn validate_query_stage_summary_fields(entries: &[(String, SemanticValue)]) -> CaapResult<()> {
    required_query_stage_summary_field(entries, "stage", semantic_non_empty_string)?;
    required_query_stage_summary_field(entries, "unit", semantic_non_empty_string)?;
    required_query_stage_summary_field(entries, "phase", semantic_phase_policy)?;
    required_query_stage_summary_field(entries, "unit_version", semantic_usize)?;
    required_query_stage_summary_field(entries, "cache_written_at_unix_ns", semantic_positive_i64)?;
    let providers =
        required_query_stage_summary_field(entries, "providers", semantic_unique_string_list)?;
    let provider_count =
        required_query_stage_summary_field(entries, "provider_count", semantic_usize)?;
    if providers.len() != provider_count {
        return Err(query_stage_summary_field_error(
            "provider_count",
            "equal to providers length",
        ));
    }
    required_query_stage_summary_field(entries, "effect_tags", semantic_unique_string_list)?;
    required_query_stage_summary_field(entries, "reads_subjects", semantic_unique_string_list)?;
    required_query_stage_summary_field(entries, "writes_subjects", semantic_unique_string_list)?;
    required_query_stage_summary_field(entries, "read_cells", semantic_unique_string_list)?;
    required_query_stage_summary_field(entries, "write_cells", semantic_unique_string_list)?;
    required_query_stage_summary_field(entries, "reads_files", semantic_unique_string_list)?;
    required_query_stage_summary_field(entries, "writes_files", semantic_unique_string_list)?;
    required_query_stage_summary_field(entries, "restarted", semantic_bool)?;
    required_query_stage_summary_field(entries, "restart_target", semantic_optional_string)?;
    required_query_stage_summary_field(entries, "artifact_key", semantic_string)?;
    required_query_stage_summary_field(
        entries,
        "execution_summary",
        semantic_execution_summary_list,
    )?;
    Ok(())
}

fn validate_query_stage_summary_record_consistency(
    entries: &[(String, SemanticValue)],
    records: &[QueryProviderExecutionRecord],
) -> CaapResult<()> {
    let stage = required_query_stage_summary_field(entries, "stage", semantic_non_empty_string)?;
    if records.iter().any(|record| record.stage != stage) {
        return Err(query_stage_summary_field_error(
            "stage",
            "consistent with all execution_summary stages",
        ));
    }

    let phase = required_query_stage_summary_field(entries, "phase", semantic_phase_policy)?;
    if records.iter().any(|record| record.phase_policy != phase) {
        return Err(query_stage_summary_field_error(
            "phase",
            "consistent with all execution_summary phase policies",
        ));
    }

    let providers =
        required_query_stage_summary_field(entries, "providers", semantic_unique_string_list)?;
    let record_providers = records
        .iter()
        .map(|record| record.provider_name.clone())
        .collect::<Vec<_>>();
    if providers != record_providers {
        return Err(query_stage_summary_field_error(
            "providers",
            "consistent with execution_summary provider order",
        ));
    }

    let effect_tags =
        required_query_stage_summary_field(entries, "effect_tags", semantic_unique_string_list)?;
    let record_effect_tags = EffectSet::from_string_set(
        records
            .iter()
            .flat_map(|record| record.effect_tags.iter_strs().map(str::to_string)),
        "cached query stage summary effect tag",
    )?
    .iter_strs()
    .map(str::to_string)
    .collect::<Vec<_>>();
    if effect_tags != record_effect_tags {
        return Err(query_stage_summary_field_error(
            "effect_tags",
            "consistent with execution_summary effect tags",
        ));
    }

    validate_query_stage_summary_string_projection(entries, "reads_subjects", records, |record| {
        &record.reads_subjects
    })?;
    validate_query_stage_summary_string_projection(
        entries,
        "writes_subjects",
        records,
        |record| &record.writes_subjects,
    )?;
    validate_query_stage_summary_string_projection(entries, "read_cells", records, |record| {
        &record.read_cells
    })?;
    validate_query_stage_summary_string_projection(entries, "write_cells", records, |record| {
        &record.write_cells
    })?;
    validate_query_stage_summary_string_projection(entries, "reads_files", records, |record| {
        &record.reads_files
    })?;
    validate_query_stage_summary_string_projection(entries, "writes_files", records, |record| {
        &record.writes_files
    })?;
    Ok(())
}

fn validate_query_stage_summary_request(
    entries: &[(String, SemanticValue)],
    request: CachedQueryStageReplayRequest<'_>,
) -> CaapResult<()> {
    let stage = required_query_stage_summary_field(entries, "stage", semantic_non_empty_string)?;
    if stage != request.stage {
        return Err(query_stage_summary_field_error(
            "stage",
            &format!("requested stage '{}'", request.stage),
        ));
    }

    let phase = required_query_stage_summary_field(entries, "phase", semantic_phase_policy)?;
    if phase != request.phase {
        return Err(query_stage_summary_field_error(
            "phase",
            &format!("requested phase '{}'", request.phase.as_str()),
        ));
    }

    let unit_id = required_query_stage_summary_field(entries, "unit", semantic_non_empty_string)?;
    if unit_id != request.unit_id {
        return Err(query_stage_summary_field_error(
            "unit",
            &format!("requested unit '{}'", request.unit_id),
        ));
    }

    Ok(())
}

fn validate_query_stage_summary_string_projection(
    entries: &[(String, SemanticValue)],
    field: &str,
    records: &[QueryProviderExecutionRecord],
    selector: impl Fn(&QueryProviderExecutionRecord) -> &Vec<String>,
) -> CaapResult<()> {
    let summary = required_query_stage_summary_field(entries, field, semantic_unique_string_list)?;
    let projected = collect_record_strings(records, selector);
    if summary != projected {
        return Err(query_stage_summary_field_error(
            field,
            "consistent with execution_summary projection",
        ));
    }
    Ok(())
}

fn required_query_stage_summary_field<T>(
    entries: &[(String, SemanticValue)],
    key: &str,
    decode: impl Fn(&SemanticValue) -> Result<T, String>,
) -> CaapResult<T> {
    let value = semantic_map_get(entries, key).ok_or_else(|| {
        query_stage_summary_field_error(key, "present exactly once in the summary")
    })?;
    decode(value).map_err(|expected| query_stage_summary_field_error(key, &expected))
}

fn query_stage_summary_field_error(key: &str, expected: &str) -> CaapError {
    CaapError::compiler(format!(
        "cached query stage artifact summary field '{key}' must be {expected}"
    ))
}

fn reject_unknown_or_duplicate_semantic_fields(
    entries: &[(String, SemanticValue)],
    allowed: &[&str],
    record_index: usize,
) -> CaapResult<()> {
    let mut seen = BTreeSet::new();
    for (key, _) in entries {
        if !allowed.contains(&key.as_str()) {
            return Err(CaapError::compiler(format!(
                "cached query execution record[{record_index}] contains unknown field '{key}'"
            )));
        }
        if !seen.insert(key.as_str()) {
            return Err(cached_record_error(
                record_index,
                key,
                "present exactly once in the record",
            ));
        }
    }
    Ok(())
}

fn required_semantic_field<T>(
    entries: &[(String, SemanticValue)],
    key: &str,
    record_index: usize,
    decode: impl Fn(&SemanticValue) -> Result<T, String>,
) -> CaapResult<T> {
    let value = semantic_map_get(entries, key).ok_or_else(|| {
        cached_record_error(record_index, key, "present exactly once in the record")
    })?;
    decode(value).map_err(|expected| cached_record_error(record_index, key, &expected))
}

fn cached_record_error(record_index: usize, key: &str, expected: &str) -> CaapError {
    CaapError::compiler(format!(
        "cached query execution record[{record_index}] field '{key}' must be {expected}"
    ))
}

pub(super) fn semantic_string_list(values: &[String]) -> SemanticValue {
    SemanticValue::List(values.iter().cloned().map(SemanticValue::Str).collect())
}

pub(super) fn semantic_effect_set(values: &EffectSet) -> SemanticValue {
    SemanticValue::List(
        values
            .iter_strs()
            .map(|value| SemanticValue::Str(value.to_string()))
            .collect(),
    )
}

fn semantic_artifact_key_list(values: &[ArtifactKey]) -> SemanticValue {
    SemanticValue::List(
        values
            .iter()
            .map(|key| {
                SemanticValue::List(
                    key.parts()
                        .iter()
                        .cloned()
                        .map(SemanticValue::Str)
                        .collect(),
                )
            })
            .collect(),
    )
}

fn optional_semantic_string(value: Option<&str>) -> SemanticValue {
    value
        .map(|value| SemanticValue::Str(value.to_string()))
        .unwrap_or(SemanticValue::Null)
}

fn semantic_map_get<'a>(
    entries: &'a [(String, SemanticValue)],
    key: &str,
) -> Option<&'a SemanticValue> {
    let mut found = None;
    for (entry_key, value) in entries {
        if entry_key != key {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(value);
    }
    found
}

fn semantic_string(value: &SemanticValue) -> Result<String, String> {
    match value {
        SemanticValue::Str(value) => Ok(value.clone()),
        _ => Err("a string".to_string()),
    }
}

fn semantic_non_empty_string(value: &SemanticValue) -> Result<String, String> {
    let value = semantic_string(value)?;
    if value.is_empty() {
        return Err("a non-empty string".to_string());
    }
    Ok(value)
}

fn semantic_optional_string(value: &SemanticValue) -> Result<Option<String>, String> {
    match value {
        SemanticValue::Null => Ok(None),
        SemanticValue::Str(value) => Ok(Some(value.clone())),
        _ => Err("null or a string".to_string()),
    }
}

fn semantic_string_list_field(value: &SemanticValue) -> Result<Vec<String>, String> {
    match value {
        SemanticValue::List(values) => values
            .iter()
            .map(|value| match value {
                SemanticValue::Str(value) => Ok(value.clone()),
                _ => Err("a list of strings".to_string()),
            })
            .collect(),
        _ => Err("a list of strings".to_string()),
    }
}

fn semantic_unique_string_list(value: &SemanticValue) -> Result<Vec<String>, String> {
    let values = semantic_string_list_field(value)?;
    let mut seen = BTreeSet::new();
    for value in &values {
        if value.is_empty() || !seen.insert(value.clone()) {
            return Err("a list of unique non-empty strings".to_string());
        }
    }
    Ok(values)
}

fn semantic_execution_summary_list(value: &SemanticValue) -> Result<(), String> {
    match value {
        SemanticValue::List(_) => Ok(()),
        _ => Err("a list of provider execution records".to_string()),
    }
}

fn semantic_artifact_key_list_field(value: &SemanticValue) -> Result<Vec<ArtifactKey>, String> {
    match value {
        SemanticValue::List(values) => values
            .iter()
            .map(|value| match value {
                SemanticValue::List(parts) => {
                    let parts: Result<Vec<String>, String> = parts
                        .iter()
                        .map(|part| match part {
                            SemanticValue::Str(part) => Ok(part.clone()),
                            _ => Err("a list of artifact-key string segments".to_string()),
                        })
                        .collect();
                    ArtifactKey::new(parts?).map_err(|error| error.to_string())
                }
                _ => Err("a list of artifact keys".to_string()),
            })
            .collect(),
        _ => Err("a list of artifact keys".to_string()),
    }
}

fn semantic_unique_artifact_key_list(value: &SemanticValue) -> Result<Vec<ArtifactKey>, String> {
    let values = semantic_artifact_key_list_field(value)?;
    let mut seen = BTreeSet::new();
    for value in &values {
        if !seen.insert(value.clone()) {
            return Err("a list of unique artifact keys".to_string());
        }
    }
    Ok(values)
}

fn semantic_bool(value: &SemanticValue) -> Result<bool, String> {
    match value {
        SemanticValue::Bool(value) => Ok(*value),
        _ => Err("a boolean".to_string()),
    }
}

fn semantic_usize(value: &SemanticValue) -> Result<usize, String> {
    match value {
        SemanticValue::Int(value) if *value >= 0 => {
            usize::try_from(*value).map_err(|_| "an integer representable as usize".to_string())
        }
        SemanticValue::Int(_) => Err("a non-negative integer".to_string()),
        _ => Err("a non-negative integer".to_string()),
    }
}

fn semantic_positive_i64(value: &SemanticValue) -> Result<i64, String> {
    match value {
        SemanticValue::Int(value) if *value > 0 => Ok(*value),
        _ => Err("a positive integer".to_string()),
    }
}

fn semantic_phase_policy(value: &SemanticValue) -> Result<PhasePolicy, String> {
    let label = semantic_string(value)?;
    PhasePolicy::parse_label(&label).map_err(|_| "one of: runtime, compile_time, dual".to_string())
}

fn semantic_string_pairs(value: &SemanticValue) -> Result<Vec<(String, String)>, String> {
    match value {
        SemanticValue::Map(values) => {
            let mut seen = BTreeSet::new();
            values
                .iter()
                .map(|(key, value)| {
                    if !seen.insert(key.as_str()) {
                        return Err("a string-keyed map with unique keys".to_string());
                    }
                    match value {
                        SemanticValue::Str(value) => Ok((key.clone(), value.clone())),
                        _ => Err("a string-keyed map of string values".to_string()),
                    }
                })
                .collect()
        }
        _ => Err("a string-keyed map of string values".to_string()),
    }
}
