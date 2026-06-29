use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use crate::error::{CaapError, CaapResult};
use crate::semantic::{EffectSet, PhasePolicy};

use super::query_provider_types::{
    data_keys_for_domains, extend_available_data, normalize_cache_scope, normalize_data_keys,
    normalize_provider_domains, normalize_resume_policy, normalize_stage_name,
    normalize_unique_labels, require_non_empty_labels, NativeProviderContext, QueryPlan,
    QueryPlanStep, QueryProvider, QueryProviderCacheScope, QueryProviderCallbackOutcome,
    QueryProviderContractSpec, QueryProviderResumePolicy, QueryProviderSchedule, QueryStageSpec,
};

// ──────────────────────────────────────────────────────────────
// Registry struct
// ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct QueryProviderRegistry {
    pub(super) stages: BTreeMap<String, QueryStageSpec>,
    aliases: BTreeMap<String, String>,
    default_stage_by_family: BTreeMap<String, String>,
    input_kind_to_stage: BTreeMap<String, String>,
    providers: Vec<QueryProvider>,
    next_registration_index: u64,
    version: u64,
}

impl QueryProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_stages(&self) -> bool {
        !self.stages.is_empty()
    }

    pub fn register_stage(&mut self, spec: QueryStageSpec) -> CaapResult<()> {
        let name = normalize_stage_name(spec.name.clone())?;
        let requires = normalize_unique_labels(spec.requires, "compiler stage dependency")?;
        let aliases = normalize_unique_labels(spec.aliases, "compiler stage alias")?;
        let input_kinds = normalize_unique_labels(spec.input_kinds, "compiler stage input kind")?;
        let family_label = spec.family_label.map(normalize_stage_name).transpose()?;
        let restart_stage = spec.restart_stage.map(normalize_stage_name).transpose()?;
        for required in &requires {
            if required == &name {
                return Err(CaapError::compiler("compiler stage cannot require itself"));
            }
        }
        for alias in &aliases {
            if let Some(owner) = self.aliases.get(alias) {
                if owner != &name {
                    return Err(CaapError::compiler(format!(
                        "compiler stage alias {alias:?} is already registered for stage {owner:?}"
                    )));
                }
            }
        }
        for input_kind in &input_kinds {
            if let Some(owner) = self.input_kind_to_stage.get(input_kind) {
                if owner != &name {
                    return Err(CaapError::compiler(format!(
                        "input kind {input_kind:?} is already accepted by stage {owner:?}"
                    )));
                }
            }
        }
        let version = self.next_version()?;
        self.drop_stage_indexes(&name);
        for alias in &aliases {
            self.aliases.insert(alias.clone(), name.clone());
        }
        for input_kind in &input_kinds {
            self.input_kind_to_stage
                .insert(input_kind.clone(), name.clone());
        }
        if let Some(family) = &family_label {
            self.default_stage_by_family
                .entry(family.clone())
                .or_insert_with(|| name.clone());
        }
        self.stages.insert(
            name.clone(),
            QueryStageSpec {
                name,
                requires,
                phase_policy: spec.phase_policy,
                input_kinds,
                family_label,
                aliases,
                restart_stage,
            },
        );
        self.version = version;
        Ok(())
    }

    fn drop_stage_indexes(&mut self, stage: &str) {
        self.aliases.retain(|_, owner| owner != stage);
        self.input_kind_to_stage.retain(|_, owner| owner != stage);
        self.default_stage_by_family
            .retain(|_, owner| owner != stage);
    }

    pub fn register_alias(
        &mut self,
        stage: impl Into<String>,
        alias: impl Into<String>,
    ) -> CaapResult<()> {
        let stage = self.resolve_stage(stage.into())?;
        let alias = normalize_stage_name(alias.into())?;
        if let Some(owner) = self.aliases.get(&alias) {
            if owner != &stage {
                return Err(CaapError::compiler(format!(
                    "compiler stage alias {alias:?} is already registered for stage {owner:?}"
                )));
            }
        }
        let version = self.next_version()?;
        self.aliases.insert(alias.clone(), stage.clone());
        if let Some(spec) = self.stages.get_mut(&stage) {
            if !spec.aliases.contains(&alias) {
                spec.aliases.push(alias);
                spec.aliases.sort();
            }
        }
        self.version = version;
        Ok(())
    }

    pub fn register_restart_stage(
        &mut self,
        stage: impl Into<String>,
        restart_stage: impl Into<String>,
    ) -> CaapResult<()> {
        let stage = self.resolve_stage(stage.into())?;
        let restart_stage = self.resolve_stage(restart_stage.into())?;
        let version = self.next_version()?;
        if let Some(spec) = self.stages.get_mut(&stage) {
            spec.restart_stage = Some(restart_stage);
        }
        self.version = version;
        Ok(())
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    fn next_version(&self) -> CaapResult<u64> {
        self.version
            .checked_add(1)
            .ok_or_else(|| CaapError::compiler("query provider registry version overflow"))
    }

    fn next_registration_index(&self) -> CaapResult<u64> {
        self.next_registration_index
            .checked_add(1)
            .ok_or_else(|| CaapError::compiler("query provider registration index overflow"))
    }

    pub fn register_provider(
        &mut self,
        name: impl Into<String>,
        stage: impl Into<String>,
        phase_policy: PhasePolicy,
        callback: impl for<'a> Fn(&mut NativeProviderContext<'a>) -> Result<(), String> + 'static,
    ) -> CaapResult<()> {
        self.register_provider_with_effects(
            name,
            stage,
            phase_policy,
            Vec::<String>::new(),
            callback,
        )
    }

    pub fn register_provider_with_effects(
        &mut self,
        name: impl Into<String>,
        stage: impl Into<String>,
        phase_policy: PhasePolicy,
        effect_tags: impl IntoIterator<Item = String>,
        callback: impl for<'a> Fn(&mut NativeProviderContext<'a>) -> Result<(), String> + 'static,
    ) -> CaapResult<()> {
        let name = name.into();
        if name.is_empty() {
            return Err(CaapError::compiler("query provider name must be non-empty"));
        }
        if self.providers.iter().any(|provider| provider.name == name) {
            return Err(CaapError::compiler(format!(
                "query provider already registered: {name}"
            )));
        }
        let stage = self.resolve_stage(stage.into())?;
        let effect_tags = EffectSet::from_unique_strings(effect_tags, "query provider effect tag")?;
        let registration_index = self.next_registration_index;
        let next_registration_index = self.next_registration_index()?;
        let version = self.next_version()?;
        self.providers.push(QueryProvider {
            name,
            stage,
            family: None,
            phase_policy,
            requires: Vec::new(),
            requires_data: Vec::new(),
            provides_data: Vec::new(),
            provides: Vec::new(),
            effect_tags,
            input_schema: None,
            reads: Vec::new(),
            writes: Vec::new(),
            cache_scope: QueryProviderCacheScope::None,
            resume_policy: QueryProviderResumePolicy::Safe,
            registration_index,
            enforce_effect_postconditions: true,
            callback: Rc::new(move |context| {
                callback(context).map(|()| QueryProviderCallbackOutcome::default())
            }),
        });
        self.next_registration_index = next_registration_index;
        self.version = version;
        Ok(())
    }

    pub fn register_provider_contract(
        &mut self,
        contract: QueryProviderContractSpec,
        callback: impl for<'a> Fn(&mut NativeProviderContext<'a>) -> Result<(), String> + 'static,
    ) -> CaapResult<()> {
        self.register_provider_contract_with_outcome(contract, move |context| {
            callback(context).map(|()| QueryProviderCallbackOutcome::default())
        })
    }

    pub fn register_provider_contract_with_outcome(
        &mut self,
        contract: QueryProviderContractSpec,
        callback: impl for<'a> Fn(
                &mut NativeProviderContext<'a>,
            ) -> Result<QueryProviderCallbackOutcome, String>
            + 'static,
    ) -> CaapResult<()> {
        self.register_provider_contract_with_outcome_and_effect_postconditions(
            contract, callback, true,
        )
    }

    pub(super) fn register_provider_contract_with_outcome_and_effect_postconditions(
        &mut self,
        contract: QueryProviderContractSpec,
        callback: impl for<'a> Fn(
                &mut NativeProviderContext<'a>,
            ) -> Result<QueryProviderCallbackOutcome, String>
            + 'static,
        enforce_effect_postconditions: bool,
    ) -> CaapResult<()> {
        let name = contract.name;
        if name.is_empty() {
            return Err(CaapError::compiler("query provider name must be non-empty"));
        }
        if self.providers.iter().any(|provider| provider.name == name) {
            return Err(CaapError::compiler(format!(
                "query provider already registered: {name}"
            )));
        }
        let stage = self.resolve_stage(contract.stage)?;
        let family = contract.family.map(normalize_stage_name).transpose()?;
        let requires = require_non_empty_labels(contract.requires, "query provider requirement")?;
        let effect_tags =
            EffectSet::from_unique_strings(contract.effect_tags, "query provider effect tag")?;
        let spec = contract.registration;
        let requires_data = normalize_data_keys(spec.requires_data)?;
        let explicit_provides_data = normalize_data_keys(spec.provides_data)?;
        let reads = normalize_provider_domains(spec.reads, "query provider read domain")?;
        let writes = normalize_provider_domains(spec.writes, "query provider write domain")?;
        let provides_data = if explicit_provides_data.is_empty() {
            data_keys_for_domains(&writes)?
        } else {
            explicit_provides_data
        };
        let cache_scope = normalize_cache_scope(spec.cache_scope)?;
        let resume_policy = normalize_resume_policy(spec.resume_policy)?;
        let registration_index = self.next_registration_index;
        let next_registration_index = self.next_registration_index()?;
        let version = self.next_version()?;
        self.providers.push(QueryProvider {
            name,
            stage,
            family,
            phase_policy: contract.phase_policy,
            requires,
            requires_data,
            provides_data: provides_data.clone(),
            provides: provides_data,
            effect_tags,
            input_schema: spec.input_schema,
            reads,
            writes,
            cache_scope,
            resume_policy,
            registration_index,
            enforce_effect_postconditions,
            callback: Rc::new(callback),
        });
        self.next_registration_index = next_registration_index;
        self.version = version;
        Ok(())
    }

    pub fn resolve_stage(&self, target: impl Into<String>) -> CaapResult<String> {
        if self.stages.is_empty() {
            return Err(CaapError::compiler("no compiler stages registered"));
        }
        let target = normalize_stage_name(target.into())?;
        if self.stages.contains_key(&target) {
            return Ok(target);
        }
        self.aliases
            .get(&target)
            .cloned()
            .ok_or_else(|| CaapError::compiler(format!("unsupported query target: {target}")))
    }

    pub fn stage_names(&self) -> Vec<&str> {
        self.stages.keys().map(String::as_str).collect()
    }

    pub fn stage_spec(&self, stage: &str) -> CaapResult<Option<&QueryStageSpec>> {
        let stage = self.resolve_stage(stage.to_string())?;
        Ok(self.stages.get(&stage))
    }

    pub fn default_stage_for_family(&self, family: impl Into<String>) -> CaapResult<String> {
        let family = normalize_stage_name(family.into())?;
        self.default_stage_by_family
            .get(&family)
            .cloned()
            .ok_or_else(|| {
                CaapError::compiler(format!("unsupported query provider family {family:?}"))
            })
    }

    pub fn stage_for_input_kind(&self, input_kind: impl Into<String>) -> CaapResult<String> {
        let input_kind = normalize_stage_name(input_kind.into())?;
        self.input_kind_to_stage
            .get(&input_kind)
            .cloned()
            .ok_or_else(|| {
                CaapError::compiler(format!("unsupported query input kind {input_kind:?}"))
            })
    }

    pub fn restart_stage_for(&self, stage: impl Into<String>) -> CaapResult<String> {
        let stage = self.resolve_stage(stage.into())?;
        let spec = self
            .stages
            .get(&stage)
            .ok_or_else(|| CaapError::compiler(format!("unsupported query stage: {stage}")))?;
        Ok(spec.restart_stage.clone().unwrap_or(stage))
    }

    pub fn explicit_restart_stage_for(
        &self,
        stage: impl Into<String>,
    ) -> CaapResult<Option<String>> {
        let stage = self.resolve_stage(stage.into())?;
        let spec = self
            .stages
            .get(&stage)
            .ok_or_else(|| CaapError::compiler(format!("unsupported query stage: {stage}")))?;
        Ok(spec.restart_stage.clone())
    }

    pub fn ordered_providers(&self) -> Vec<QueryProvider> {
        let mut providers = self.providers.clone();
        providers.sort_by_key(|provider| provider.registration_index);
        providers
    }

    pub fn providers_for_stage(&self, stage: impl Into<String>) -> CaapResult<Vec<QueryProvider>> {
        let stage = self.resolve_stage(stage.into())?;
        Ok(self
            .ordered_providers()
            .into_iter()
            .filter(|provider| provider.stage == stage)
            .collect())
    }

    pub(super) fn provider_names_for_completed_origin(
        &self,
        origin_stage: Option<&str>,
    ) -> CaapResult<BTreeSet<String>> {
        let Some(origin_stage) = origin_stage else {
            return Ok(BTreeSet::new());
        };
        let completed_stages: BTreeSet<String> =
            self.route_to_stage(origin_stage)?.into_iter().collect();
        Ok(self
            .ordered_providers()
            .into_iter()
            .filter(|provider| completed_stages.contains(&provider.stage))
            .map(|provider| provider.name)
            .collect())
    }

    pub(super) fn data_keys_for_satisfied_providers(
        &self,
        satisfied: &BTreeSet<String>,
    ) -> Vec<String> {
        let mut keys = Vec::new();
        for provider in self.ordered_providers() {
            if satisfied.contains(&provider.name) {
                extend_available_data(&mut keys, provider.provides_data.iter().cloned());
            }
        }
        keys
    }

    pub fn provider_schedule_for_stage(
        &self,
        stage: impl Into<String>,
    ) -> CaapResult<QueryProviderSchedule> {
        self.provider_schedule_for_stage_with_available_data(stage, [])
    }

    pub fn provider_schedule_for_stage_with_available_data(
        &self,
        stage: impl Into<String>,
        available_data: impl IntoIterator<Item = String>,
    ) -> CaapResult<QueryProviderSchedule> {
        self.provider_schedule_for_stage_with_dynamic_requires(
            stage,
            available_data,
            &BTreeSet::new(),
            &BTreeMap::new(),
        )
    }

    pub(super) fn provider_schedule_for_stage_with_dynamic_requires(
        &self,
        stage: impl Into<String>,
        available_data: impl IntoIterator<Item = String>,
        previously_satisfied: &BTreeSet<String>,
        dynamic_requires: &BTreeMap<String, Vec<String>>,
    ) -> CaapResult<QueryProviderSchedule> {
        let providers = self.providers_for_stage(stage)?;
        let available_data = normalize_data_keys(available_data)?;
        let observed_requires =
            observed_provider_requires(&providers, previously_satisfied, dynamic_requires);
        provider_schedule_batches(providers, &available_data, &observed_requires)
    }

    pub fn plan(&self, target: impl Into<String>, phase: PhasePolicy) -> CaapResult<QueryPlan> {
        let target = self.resolve_stage(target.into())?;
        let route = self.route_to_stage(&target)?;
        self.plan_for_route(target, route, phase)
    }

    pub fn plan_from_stage_to_target(
        &self,
        from_stage: impl Into<String>,
        target: impl Into<String>,
        phase: PhasePolicy,
    ) -> CaapResult<QueryPlan> {
        let target = self.resolve_stage(target.into())?;
        let route = self.route_from_stage_to_target(from_stage, target.clone())?;
        self.plan_for_route(target, route, phase)
    }

    pub(super) fn plan_from_origin_option(
        &self,
        from_stage: Option<&str>,
        target: impl Into<String>,
        phase: PhasePolicy,
    ) -> CaapResult<QueryPlan> {
        match from_stage {
            Some(from_stage) => self.plan_from_stage_to_target(from_stage, target, phase),
            None => self.plan(target, phase),
        }
    }

    fn plan_for_route(
        &self,
        target: String,
        route: Vec<String>,
        phase: PhasePolicy,
    ) -> CaapResult<QueryPlan> {
        let mut steps = Vec::with_capacity(route.len());
        let mut available_data = Vec::new();
        for stage in route {
            let schedule = self.provider_schedule_for_stage_with_available_data(
                stage.clone(),
                available_data.clone(),
            )?;
            let providers: Vec<QueryProvider> = schedule.groups.into_iter().flatten().collect();
            let provider_names = providers
                .iter()
                .map(|provider| provider.name.clone())
                .collect();
            let effect_tags = EffectSet::from_string_set(
                providers
                    .iter()
                    .flat_map(|provider| provider.effect_tags.iter_strs().map(str::to_string)),
                "query stage effect tag",
            )?;
            steps.push(QueryPlanStep {
                stage,
                provider_names,
                effect_tags,
                cached: false,
                artifact_key: None,
                restarted: false,
                restart_target: None,
            });
            extend_available_data(
                &mut available_data,
                providers
                    .iter()
                    .flat_map(|provider| provider.provides_data.iter().cloned()),
            );
        }
        Ok(QueryPlan {
            target,
            phase,
            steps,
            executed: Vec::new(),
        })
    }

    pub fn route_to_stage(&self, target: &str) -> CaapResult<Vec<String>> {
        let target = self.resolve_stage(target.to_string())?;
        let mut route = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        self.visit_stage(&target, &mut visiting, &mut visited, &mut route)?;
        Ok(route)
    }

    pub fn route_from_stage_to_target(
        &self,
        from_stage: impl Into<String>,
        target: impl Into<String>,
    ) -> CaapResult<Vec<String>> {
        if self.stages.is_empty() {
            return Err(CaapError::compiler("no compiler stages registered"));
        }
        let from_stage = self.resolve_stage(from_stage.into())?;
        let target = self.resolve_stage(target.into())?;
        let _ = self.route_to_stage(&target)?;
        if from_stage == target {
            return Ok(Vec::new());
        }

        let mut adjacency: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for spec in self.stages.values() {
            for dependency in &spec.requires {
                adjacency
                    .entry(dependency.as_str())
                    .or_default()
                    .push(spec.name.as_str());
            }
        }
        for dependents in adjacency.values_mut() {
            dependents.sort();
        }

        // Parent-pointer BFS: O(n) clone cost on the success path (path reconstructed
        // once on exit) instead of the previous O(n * depth) path-per-frame clone.
        let mut seen = BTreeSet::from([from_stage.clone()]);
        let mut parent: BTreeMap<String, String> = BTreeMap::new();
        let mut queue = VecDeque::from([from_stage.clone()]);
        while let Some(stage) = queue.pop_front() {
            for candidate in adjacency.get(stage.as_str()).into_iter().flatten() {
                let candidate_owned = (*candidate).to_string();
                if !seen.insert(candidate_owned.clone()) {
                    continue;
                }
                parent
                    .entry(candidate_owned.clone())
                    .or_insert_with(|| stage.clone());
                if *candidate == target.as_str() {
                    let mut path = vec![candidate_owned.clone()];
                    let mut node = candidate_owned;
                    loop {
                        let pred = parent.get(&node).expect("parent map complete");
                        if *pred == from_stage {
                            break;
                        }
                        path.push(pred.clone());
                        node = pred.clone();
                    }
                    path.reverse();
                    return Ok(path);
                }
                queue.push_back(candidate_owned);
            }
        }
        Err(CaapError::compiler(format!(
            "cannot schedule compiler from {from_stage:?} to {target:?}"
        )))
    }

    fn visit_stage(
        &self,
        stage: &str,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        route: &mut Vec<String>,
    ) -> CaapResult<()> {
        if visited.contains(stage) {
            return Ok(());
        }
        if !visiting.insert(stage.to_string()) {
            return Err(CaapError::compiler(format!(
                "compiler stage graph contains a cycle at {stage}"
            )));
        }
        let spec = self
            .stages
            .get(stage)
            .ok_or_else(|| CaapError::compiler(format!("unsupported query stage: {stage}")))?;
        for required in &spec.requires {
            if !self.stages.contains_key(required) {
                return Err(CaapError::compiler(format!(
                    "compiler stage {stage:?} depends on missing stage {required:?}"
                )));
            }
            self.visit_stage(required, visiting, visited, route)?;
        }
        visiting.remove(stage);
        visited.insert(stage.to_string());
        route.push(stage.to_string());
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────
// Scheduling helpers (DAG / topological sort / reachability)
// ──────────────────────────────────────────────────────────────

pub(super) fn observed_provider_requires(
    providers: &[QueryProvider],
    previously_satisfied: &BTreeSet<String>,
    dynamic_requires: &BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    let stage_provider_names: BTreeSet<String> = providers
        .iter()
        .map(|provider| provider.name.clone())
        .collect();
    providers
        .iter()
        .map(|provider| {
            let mut requirements: Vec<String> = dynamic_requires
                .get(&provider.name)
                .into_iter()
                .flatten()
                .filter(|requirement| {
                    stage_provider_names.contains(*requirement)
                        || previously_satisfied.contains(*requirement)
                })
                .cloned()
                .collect();
            requirements.sort();
            requirements.dedup();
            (provider.name.clone(), requirements)
        })
        .collect()
}

fn provider_schedule_batches(
    providers: Vec<QueryProvider>,
    available_data: &[String],
    dynamic_requires: &BTreeMap<String, Vec<String>>,
) -> CaapResult<QueryProviderSchedule> {
    if providers.is_empty() {
        return Ok(QueryProviderSchedule {
            groups: Vec::new(),
            barriers: Vec::new(),
        });
    }

    let positions: BTreeMap<String, usize> = providers
        .iter()
        .enumerate()
        .map(|(index, provider)| (provider.name.clone(), index))
        .collect();
    let by_name: BTreeMap<String, QueryProvider> = providers
        .iter()
        .map(|provider| (provider.name.clone(), provider.clone()))
        .collect();
    let mut outgoing: BTreeMap<String, BTreeSet<String>> = providers
        .iter()
        .map(|provider| (provider.name.clone(), BTreeSet::new()))
        .collect();
    let mut incoming_count: BTreeMap<String, usize> = providers
        .iter()
        .map(|provider| (provider.name.clone(), 0))
        .collect();

    for provider in &providers {
        for requirement in &provider.requires {
            if !positions.contains_key(requirement) {
                return Err(CaapError::compiler(format!(
                    "provider {:?} requires missing provider {:?}",
                    provider.name, requirement
                )));
            }
            add_provider_schedule_edge(
                &mut outgoing,
                &mut incoming_count,
                requirement,
                &provider.name,
            );
        }
        for requirement in dynamic_requires
            .get(&provider.name)
            .into_iter()
            .flatten()
            .filter(|requirement| positions.contains_key(*requirement))
        {
            add_provider_schedule_edge(
                &mut outgoing,
                &mut incoming_count,
                requirement,
                &provider.name,
            );
        }
    }
    add_provider_data_edges(
        &providers,
        &positions,
        &mut outgoing,
        &mut incoming_count,
        available_data,
    )?;
    add_provider_effect_conflict_edges(&providers, &mut outgoing, &mut incoming_count);

    let mut ready: Vec<String> = providers
        .iter()
        .filter(|provider| incoming_count.get(&provider.name).copied().unwrap_or(0) == 0)
        .map(|provider| provider.name.clone())
        .collect();
    ready.sort_by_key(|name| positions[name]);

    let mut visited = BTreeSet::new();
    let mut batches = Vec::new();
    while !ready.is_empty() {
        let batch_names = std::mem::take(&mut ready);
        let mut batch = Vec::with_capacity(batch_names.len());
        let mut next = Vec::new();
        for name in batch_names {
            if !visited.insert(name.clone()) {
                continue;
            }
            batch.push(by_name.get(&name).cloned().ok_or_else(|| {
                CaapError::compiler(format!("provider scheduler lost provider {name:?}"))
            })?);
            for target in outgoing.get(&name).into_iter().flatten() {
                let Some(count) = incoming_count.get_mut(target) else {
                    continue;
                };
                *count = count.saturating_sub(1);
                if *count == 0 {
                    next.push(target.clone());
                }
            }
        }
        if !batch.is_empty() {
            batches.push(batch);
        }
        next.sort_by_key(|name| positions[name]);
        next.dedup();
        ready = next;
    }

    if visited.len() != providers.len() {
        return Err(CaapError::compiler(
            "provider scheduler detected a cycle inside one stage",
        ));
    }

    let barriers = provider_schedule_barriers(&batches);
    Ok(QueryProviderSchedule {
        groups: batches,
        barriers,
    })
}

fn add_provider_schedule_edge(
    outgoing: &mut BTreeMap<String, BTreeSet<String>>,
    incoming_count: &mut BTreeMap<String, usize>,
    before: &str,
    after: &str,
) {
    if before == after {
        return;
    }
    let Some(targets) = outgoing.get_mut(before) else {
        return;
    };
    if targets.insert(after.to_string()) {
        *incoming_count.entry(after.to_string()).or_default() += 1;
    }
}

fn add_provider_data_edges(
    providers: &[QueryProvider],
    positions: &BTreeMap<String, usize>,
    outgoing: &mut BTreeMap<String, BTreeSet<String>>,
    incoming_count: &mut BTreeMap<String, usize>,
    available_data: &[String],
) -> CaapResult<()> {
    for provider in providers {
        for requirement in &provider.requires_data {
            if available_data
                .iter()
                .any(|key| data_key_matches(requirement, key))
            {
                continue;
            }
            let mut suppliers: Vec<&str> = providers
                .iter()
                .filter(|candidate| candidate.name != provider.name)
                .filter(|candidate| {
                    candidate
                        .provides_data
                        .iter()
                        .any(|provided| data_key_matches(requirement, provided))
                })
                .map(|candidate| candidate.name.as_str())
                .collect();
            suppliers.sort_by_key(|name| positions[*name]);
            if suppliers.is_empty() {
                return Err(CaapError::compiler(format!(
                    "provider {:?} requires data {:?} from a later or missing stage",
                    provider.name, requirement
                )));
            }
            for supplier in suppliers {
                add_provider_schedule_edge(outgoing, incoming_count, supplier, &provider.name);
            }
        }
    }
    Ok(())
}

fn add_provider_effect_conflict_edges(
    providers: &[QueryProvider],
    outgoing: &mut BTreeMap<String, BTreeSet<String>>,
    incoming_count: &mut BTreeMap<String, usize>,
) {
    for (index, current) in providers.iter().enumerate() {
        if current.reads.is_empty() && current.writes.is_empty() {
            continue;
        }
        for later in providers.iter().skip(index + 1) {
            if !provider_effects_conflict(current, later) {
                continue;
            }
            let reachable = provider_schedule_reachability(providers, outgoing);
            if reachable
                .get(&later.name)
                .is_some_and(|targets| targets.contains(&current.name))
            {
                continue;
            }
            add_provider_schedule_edge(outgoing, incoming_count, &current.name, &later.name);
        }
    }
}

fn provider_effects_conflict(current: &QueryProvider, later: &QueryProvider) -> bool {
    strings_intersect(&current.reads, &later.writes)
        || strings_intersect(&current.writes, &later.reads)
        || strings_intersect(&current.writes, &later.writes)
}

fn provider_schedule_reachability(
    providers: &[QueryProvider],
    outgoing: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut reachable = BTreeMap::new();
    for provider in providers {
        let mut visited = BTreeSet::new();
        let mut stack: Vec<String> = outgoing
            .get(&provider.name)
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        while let Some(name) = stack.pop() {
            if !visited.insert(name.clone()) {
                continue;
            }
            stack.extend(outgoing.get(&name).into_iter().flatten().cloned());
        }
        reachable.insert(provider.name.clone(), visited);
    }
    reachable
}

fn provider_schedule_barriers(groups: &[Vec<QueryProvider>]) -> Vec<Option<Vec<String>>> {
    let mut barriers = Vec::with_capacity(groups.len());
    for index in 0..groups.len() {
        let Some(next_group) = groups.get(index + 1) else {
            barriers.push(None);
            continue;
        };
        let mut reasons = Vec::new();
        for current in &groups[index] {
            for later in next_group {
                push_provider_barrier_reason(
                    &mut reasons,
                    "writes after reads on",
                    &current.reads,
                    &later.writes,
                );
                push_provider_barrier_reason(
                    &mut reasons,
                    "reads after writes on",
                    &current.writes,
                    &later.reads,
                );
                push_provider_barrier_reason(
                    &mut reasons,
                    "competing writes on",
                    &current.writes,
                    &later.writes,
                );
            }
        }
        reasons.sort();
        reasons.dedup();
        barriers.push((!reasons.is_empty()).then_some(reasons));
    }
    barriers
}

fn push_provider_barrier_reason(
    reasons: &mut Vec<String>,
    label: &str,
    left: &[String],
    right: &[String],
) {
    let intersection = string_intersection(left, right);
    if !intersection.is_empty() {
        reasons.push(format!("{label} {}", intersection.join(", ")));
    }
}

fn strings_intersect(left: &[String], right: &[String]) -> bool {
    left.iter().any(|item| right.contains(item))
}

fn string_intersection(left: &[String], right: &[String]) -> Vec<String> {
    let mut values: Vec<String> = left
        .iter()
        .filter(|item| right.contains(item))
        .cloned()
        .collect();
    values.sort();
    values.dedup();
    values
}

fn data_key_matches(requirement: &str, provided: &str) -> bool {
    requirement == provided
        || requirement
            .strip_suffix(".*")
            .is_some_and(|prefix| provided.starts_with(&format!("{prefix}.")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_stage_rejects_version_overflow_without_mutating() {
        let mut registry = QueryProviderRegistry {
            version: u64::MAX,
            ..QueryProviderRegistry::new()
        };

        let error = registry
            .register_stage(QueryStageSpec::new("check").unwrap())
            .unwrap_err()
            .to_string();

        assert!(error.contains("query provider registry version overflow"));
        assert!(registry.stages.is_empty());
        assert_eq!(registry.version, u64::MAX);
    }

    #[test]
    fn register_provider_rejects_registration_index_overflow_without_mutating() {
        let mut registry = QueryProviderRegistry::new();
        registry
            .register_stage(QueryStageSpec::new("check").unwrap())
            .unwrap();
        registry.next_registration_index = u64::MAX;
        let version = registry.version;

        let error = registry
            .register_provider("provider", "check", PhasePolicy::CompileTime, |_| Ok(()))
            .unwrap_err()
            .to_string();

        assert!(error.contains("query provider registration index overflow"));
        assert!(registry.providers.is_empty());
        assert_eq!(registry.next_registration_index, u64::MAX);
        assert_eq!(registry.version, version);
    }

    #[test]
    fn register_provider_rejects_version_overflow_without_mutating() {
        let mut registry = QueryProviderRegistry::new();
        registry
            .register_stage(QueryStageSpec::new("check").unwrap())
            .unwrap();
        registry.version = u64::MAX;

        let error = registry
            .register_provider("provider", "check", PhasePolicy::CompileTime, |_| Ok(()))
            .unwrap_err()
            .to_string();

        assert!(error.contains("query provider registry version overflow"));
        assert!(registry.providers.is_empty());
        assert_eq!(registry.next_registration_index, 0);
        assert_eq!(registry.version, u64::MAX);
    }
}
