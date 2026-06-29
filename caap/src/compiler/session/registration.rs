//! Stage and provider registration for [`Compiler`].
//!
//! The CTFE bootstrap registers the compiler's stage graph and query providers
//! through these entry points. Split out of `session/mod.rs` so the
//! composition root keeps only core session accessors and service entry points.

use std::rc::Rc;

use crate::compiler::query_provider::{
    normalize_stage_name, NativeProviderContext, QueryProviderCallbackOutcome,
    QueryProviderContractSpec, QueryProviderRegistrationSpec, QueryStageSpec,
};
use crate::error::CaapResult;
use crate::semantic::PhasePolicy;

use super::Compiler;

impl Compiler {
    pub fn register_stage(&mut self, stage: impl Into<String>) -> CaapResult<()> {
        let stage = normalize_stage_name(stage.into())?;
        Rc::make_mut(&mut self.dispatch.registry)
            .register_stage(QueryStageSpec::new(stage.clone())?)?;
        self.advance_session_version()?;
        Ok(())
    }

    pub fn register_stage_spec(&mut self, spec: QueryStageSpec) -> CaapResult<()> {
        Rc::make_mut(&mut self.dispatch.registry).register_stage(spec)?;
        self.advance_session_version()?;
        Ok(())
    }

    pub fn register_stage_alias(
        &mut self,
        stage: impl Into<String>,
        alias: impl Into<String>,
    ) -> CaapResult<()> {
        Rc::make_mut(&mut self.dispatch.registry).register_alias(stage, alias)?;
        self.advance_session_version()?;
        Ok(())
    }

    pub fn register_stage_restart_policy(
        &mut self,
        stage: impl Into<String>,
        restart_stage: impl Into<String>,
    ) -> CaapResult<()> {
        Rc::make_mut(&mut self.dispatch.registry).register_restart_stage(stage, restart_stage)?;
        self.advance_session_version()?;
        Ok(())
    }

    pub fn registered_stages(&self) -> Vec<&str> {
        self.dispatch.registry.stage_names()
    }

    pub fn register_provider(
        &mut self,
        name: impl Into<String>,
        stage: impl Into<String>,
        phase_policy: PhasePolicy,
        callback: impl for<'a> Fn(&mut NativeProviderContext<'a>) -> Result<(), String> + 'static,
    ) -> CaapResult<()> {
        Rc::make_mut(&mut self.dispatch.registry).register_provider(
            name,
            stage,
            phase_policy,
            callback,
        )?;
        self.advance_session_version()?;
        Ok(())
    }

    pub fn register_provider_with_effects(
        &mut self,
        name: impl Into<String>,
        stage: impl Into<String>,
        phase_policy: PhasePolicy,
        effect_tags: impl IntoIterator<Item = String>,
        callback: impl for<'a> Fn(&mut NativeProviderContext<'a>) -> Result<(), String> + 'static,
    ) -> CaapResult<()> {
        Rc::make_mut(&mut self.dispatch.registry).register_provider_with_effects(
            name,
            stage,
            phase_policy,
            effect_tags,
            callback,
        )?;
        self.advance_session_version()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn register_provider_contract(
        &mut self,
        name: impl Into<String>,
        stage: impl Into<String>,
        family: Option<String>,
        phase_policy: PhasePolicy,
        requires: impl IntoIterator<Item = String>,
        effect_tags: impl IntoIterator<Item = String>,
        registration: QueryProviderRegistrationSpec,
        callback: impl for<'a> Fn(&mut NativeProviderContext<'a>) -> Result<(), String> + 'static,
    ) -> CaapResult<()> {
        self.register_provider_contract_spec(
            QueryProviderContractSpec {
                name: name.into(),
                stage: stage.into(),
                family,
                phase_policy,
                requires: requires.into_iter().collect(),
                effect_tags: effect_tags.into_iter().collect(),
                registration,
            },
            callback,
        )
    }

    pub fn register_provider_contract_spec(
        &mut self,
        contract: QueryProviderContractSpec,
        callback: impl for<'a> Fn(&mut NativeProviderContext<'a>) -> Result<(), String> + 'static,
    ) -> CaapResult<()> {
        Rc::make_mut(&mut self.dispatch.registry).register_provider_contract(contract, callback)?;
        self.advance_session_version()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn register_provider_contract_with_outcome(
        &mut self,
        name: impl Into<String>,
        stage: impl Into<String>,
        family: Option<String>,
        phase_policy: PhasePolicy,
        requires: impl IntoIterator<Item = String>,
        effect_tags: impl IntoIterator<Item = String>,
        registration: QueryProviderRegistrationSpec,
        callback: impl for<'a> Fn(
                &mut NativeProviderContext<'a>,
            ) -> Result<QueryProviderCallbackOutcome, String>
            + 'static,
    ) -> CaapResult<()> {
        self.register_provider_contract_spec_with_outcome(
            QueryProviderContractSpec {
                name: name.into(),
                stage: stage.into(),
                family,
                phase_policy,
                requires: requires.into_iter().collect(),
                effect_tags: effect_tags.into_iter().collect(),
                registration,
            },
            callback,
        )
    }

    pub fn register_provider_contract_spec_with_outcome(
        &mut self,
        contract: QueryProviderContractSpec,
        callback: impl for<'a> Fn(
                &mut NativeProviderContext<'a>,
            ) -> Result<QueryProviderCallbackOutcome, String>
            + 'static,
    ) -> CaapResult<()> {
        Rc::make_mut(&mut self.dispatch.registry)
            .register_provider_contract_with_outcome(contract, callback)?;
        self.advance_session_version()?;
        Ok(())
    }
}
