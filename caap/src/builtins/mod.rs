pub mod arithmetic;
pub mod compiler_graphs;
pub mod compiler_providers;
pub mod compiler_query;
pub mod compiler_registry;
pub mod compiler_units;
pub mod control_flow;
pub mod host_services;
pub mod ir_builders;
pub mod mutable;
pub mod provider_context;
pub mod reflect;
pub mod sequences;
pub mod strings;
pub mod surface;

pub fn register_all(ev: &mut crate::eval::Evaluator) {
    arithmetic::register(ev);
    control_flow::register(ev);
    mutable::register(ev);
    strings::register(ev);
    sequences::register(ev);
    reflect::register(ev);
    host_services::register(ev);
    compiler_registry::register(ev);
    compiler_providers::register(ev);
    compiler_graphs::register(ev);
    compiler_query::register(ev);
    compiler_units::register(ev);
    ir_builders::register(ev);
    provider_context::register(ev);
    surface::register(ev);
}
