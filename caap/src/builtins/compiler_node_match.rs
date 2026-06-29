//! Declarative CTFE IR tree matching.
//!
//! `ctfe-node-match` is intentionally a data-driven mechanism: stdlib passes
//! supply pattern maps, while the kernel only knows how to project IR tree
//! structure and return explicit bindings.
use indexmap::IndexMap;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::bridges::NodeBridgeValue;
use crate::builtins::ir_builders::ExprSpecBridgeValue;
use crate::ir::{ExprSpec, Node};
use crate::unit::Unit;
use crate::values::{
    eval_err, runtime_value_from_literal, EvalSignal, HostObject, MapKey, RuntimeValue,
};

use super::compiler_units_helpers::{
    expr_spec_bridge, expr_spec_children, expr_spec_kind_label, map, node_handle_from_live_node_id,
    node_kind_label, spec_value, string, with_node,
};

#[derive(Default)]
struct NodeMatchState {
    bindings: IndexMap<MapKey, RuntimeValue>,
}

impl NodeMatchState {
    fn bind(&mut self, name: &str, value: RuntimeValue) -> Result<bool, EvalSignal> {
        if name.is_empty() {
            return Err(eval_err(
                "ctfe-node-match bind names must be non-empty strings",
            ));
        }
        let key = MapKey::Str(name.into());
        match self.bindings.get(&key) {
            Some(existing) => Ok(runtime_values_equivalent(existing, &value)),
            None => {
                self.bindings.insert(key, value);
                Ok(true)
            }
        }
    }

    fn result(self, matched: bool) -> RuntimeValue {
        let bindings = if matched {
            self.bindings
        } else {
            IndexMap::new()
        };
        map([
            ("matched", RuntimeValue::Bool(matched)),
            (
                "bindings",
                RuntimeValue::Map(Rc::new(RefCell::new(bindings))),
            ),
        ])
    }
}

pub(super) fn ctfe_node_match(
    value: &RuntimeValue,
    pattern: &RuntimeValue,
) -> Result<RuntimeValue, EvalSignal> {
    let mut state = NodeMatchState::default();
    let matched = if let Some(spec) = expr_spec_bridge(value) {
        node_match_spec(&spec.spec(), pattern, &mut state)?
    } else {
        with_node(
            value,
            "ctfe-node-match expects a live node or detached node spec",
            |node_handle, unit, node| {
                node_match_live(&node_handle.unit, unit, node, pattern, &mut state)
            },
        )?
    };
    Ok(state.result(matched))
}

fn runtime_values_equivalent(left: &RuntimeValue, right: &RuntimeValue) -> bool {
    match (left, right) {
        (RuntimeValue::Map(left), RuntimeValue::Map(right)) => {
            let left = left.borrow();
            let right = right.borrow();
            left.len() == right.len()
                && left.iter().all(|(key, value)| {
                    right
                        .get(key)
                        .is_some_and(|right_value| runtime_values_equivalent(value, right_value))
                })
        }
        (RuntimeValue::List(left), RuntimeValue::List(right)) => {
            let left = left.borrow();
            let right = right.borrow();
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right.iter())
                    .all(|(left, right)| runtime_values_equivalent(left, right))
        }
        (RuntimeValue::Tuple(left), RuntimeValue::Tuple(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right.iter())
                    .all(|(left, right)| runtime_values_equivalent(left, right))
        }
        (RuntimeValue::HostObject(left), RuntimeValue::HostObject(right)) => {
            if let (Some(left), Some(right)) = (
                left.as_any().downcast_ref::<NodeBridgeValue>(),
                right.as_any().downcast_ref::<NodeBridgeValue>(),
            ) {
                return left.node_id == right.node_id && Rc::ptr_eq(&left.unit, &right.unit);
            }
            if let (Some(left), Some(right)) = (
                left.as_any().downcast_ref::<ExprSpecBridgeValue>(),
                right.as_any().downcast_ref::<ExprSpecBridgeValue>(),
            ) {
                return left.clone_spec() == right.clone_spec();
            }
            Rc::ptr_eq(left, right)
        }
        _ => left == right,
    }
}

fn node_match_pattern_map(
    pattern: &RuntimeValue,
) -> Result<Option<HashMap<String, RuntimeValue>>, EvalSignal> {
    match pattern {
        RuntimeValue::Null => Ok(None),
        RuntimeValue::Map(map) => {
            let mut fields = HashMap::new();
            for (key, value) in map.borrow().iter() {
                let MapKey::Str(key) = key else {
                    return Err(eval_err("ctfe-node-match pattern maps require string keys"));
                };
                let key = key.to_string();
                if ![
                    "kind",
                    "bind",
                    "identifier",
                    "bind_identifier",
                    "value",
                    "bind_value",
                    "callee",
                    "args",
                    "children",
                ]
                .contains(&key.as_str())
                {
                    return Err(eval_err(format!(
                        "ctfe-node-match pattern contains unknown field '{key}'"
                    )));
                }
                fields.insert(key, value.clone());
            }
            Ok(Some(fields))
        }
        RuntimeValue::Str(_) => Ok(Some(HashMap::from([("kind".to_string(), pattern.clone())]))),
        _ => Err(eval_err(
            "ctfe-node-match pattern must be null, a kind string, or a map",
        )),
    }
}

fn node_match_string_field<'a>(
    fields: &'a HashMap<String, RuntimeValue>,
    key: &str,
) -> Result<Option<&'a str>, EvalSignal> {
    fields
        .get(key)
        .map(|value| match value {
            RuntimeValue::Str(value) if !value.is_empty() => Ok(value.as_ref()),
            RuntimeValue::Str(_) => Err(eval_err(format!(
                "ctfe-node-match pattern field '{key}' must be a non-empty string"
            ))),
            _ => Err(eval_err(format!(
                "ctfe-node-match pattern field '{key}' must be a string"
            ))),
        })
        .transpose()
}

fn node_match_sequence_field(
    fields: &HashMap<String, RuntimeValue>,
    key: &str,
) -> Result<Option<Vec<RuntimeValue>>, EvalSignal> {
    fields
        .get(key)
        .map(|value| match value {
            RuntimeValue::Tuple(items) => Ok(items.iter().cloned().collect()),
            RuntimeValue::List(items) => Ok(items.borrow().clone()),
            _ => Err(eval_err(format!(
                "ctfe-node-match pattern field '{key}' must be a list or tuple"
            ))),
        })
        .transpose()
}

fn node_match_live(
    unit_object: &Rc<dyn HostObject>,
    unit: &Unit,
    node: &Node,
    pattern: &RuntimeValue,
    state: &mut NodeMatchState,
) -> Result<bool, EvalSignal> {
    let Some(fields) = node_match_pattern_map(pattern)? else {
        return Ok(true);
    };
    let kind = node_kind_label(node);
    if node_match_string_field(&fields, "kind")?.is_some_and(|expected| expected != kind) {
        return Ok(false);
    }
    if let Some(bind) = node_match_string_field(&fields, "bind")? {
        if !state.bind(
            bind,
            node_handle_from_live_node_id(Rc::clone(unit_object), node.id()),
        )? {
            return Ok(false);
        }
    }
    if !node_match_node_payload(node, &fields, state)? {
        return Ok(false);
    }
    if let Some(callee_pattern) = fields.get("callee") {
        let Node::Call(call) = node else {
            return Ok(false);
        };
        let callee = unit
            .ir()
            .node(call.callee)
            .ok_or_else(|| eval_err(format!("unknown node id: {}", call.callee)))?;
        if !node_match_live(unit_object, unit, callee, callee_pattern, state)? {
            return Ok(false);
        }
    }
    if let Some(arg_patterns) = node_match_sequence_field(&fields, "args")? {
        let Node::Call(call) = node else {
            return Ok(false);
        };
        if arg_patterns.len() != call.args.len() {
            return Ok(false);
        }
        for (pattern, node_id) in arg_patterns.iter().zip(call.args.iter()) {
            let arg = unit
                .ir()
                .node(*node_id)
                .ok_or_else(|| eval_err(format!("unknown node id: {node_id}")))?;
            if !node_match_live(unit_object, unit, arg, pattern, state)? {
                return Ok(false);
            }
        }
    }
    if let Some(child_patterns) = node_match_sequence_field(&fields, "children")? {
        let children = node.children();
        if child_patterns.len() != children.len() {
            return Ok(false);
        }
        for (pattern, node_id) in child_patterns.iter().zip(children.iter()) {
            let child = unit
                .ir()
                .node(*node_id)
                .ok_or_else(|| eval_err(format!("unknown node id: {node_id}")))?;
            if !node_match_live(unit_object, unit, child, pattern, state)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn node_match_spec(
    spec: &ExprSpec,
    pattern: &RuntimeValue,
    state: &mut NodeMatchState,
) -> Result<bool, EvalSignal> {
    let Some(fields) = node_match_pattern_map(pattern)? else {
        return Ok(true);
    };
    let kind = expr_spec_kind_label(spec);
    if node_match_string_field(&fields, "kind")?.is_some_and(|expected| expected != kind) {
        return Ok(false);
    }
    if let Some(bind) = node_match_string_field(&fields, "bind")? {
        if !state.bind(bind, spec_value(spec.clone()))? {
            return Ok(false);
        }
    }
    if !node_match_spec_payload(spec, &fields, state)? {
        return Ok(false);
    }
    if let Some(callee_pattern) = fields.get("callee") {
        let ExprSpec::Call(call) = spec else {
            return Ok(false);
        };
        if !node_match_spec(&call.callee, callee_pattern, state)? {
            return Ok(false);
        }
    }
    if let Some(arg_patterns) = node_match_sequence_field(&fields, "args")? {
        let ExprSpec::Call(call) = spec else {
            return Ok(false);
        };
        if arg_patterns.len() != call.args.len() {
            return Ok(false);
        }
        for (pattern, arg) in arg_patterns.iter().zip(call.args.iter()) {
            if !node_match_spec(arg, pattern, state)? {
                return Ok(false);
            }
        }
    }
    if let Some(child_patterns) = node_match_sequence_field(&fields, "children")? {
        let children = expr_spec_children(spec);
        if child_patterns.len() != children.len() {
            return Ok(false);
        }
        for (pattern, child) in child_patterns.iter().zip(children.iter()) {
            if !node_match_spec(child, pattern, state)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn node_match_node_payload(
    node: &Node,
    fields: &HashMap<String, RuntimeValue>,
    state: &mut NodeMatchState,
) -> Result<bool, EvalSignal> {
    if fields.contains_key("identifier") || fields.contains_key("bind_identifier") {
        let Node::Name(name) = node else {
            return Ok(false);
        };
        if node_match_string_field(fields, "identifier")?
            .is_some_and(|expected| expected != name.identifier.as_ref())
        {
            return Ok(false);
        }
        if let Some(bind) = node_match_string_field(fields, "bind_identifier")? {
            if !state.bind(bind, string(name.identifier.as_ref()))? {
                return Ok(false);
            }
        }
    }
    if fields.contains_key("value") || fields.contains_key("bind_value") {
        let Node::Literal(literal) = node else {
            return Ok(false);
        };
        let runtime_literal = runtime_value_from_literal(&literal.value);
        if fields
            .get("value")
            .is_some_and(|expected| !runtime_values_equivalent(expected, &runtime_literal))
        {
            return Ok(false);
        }
        if let Some(bind) = node_match_string_field(fields, "bind_value")? {
            if !state.bind(bind, runtime_literal)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn node_match_spec_payload(
    spec: &ExprSpec,
    fields: &HashMap<String, RuntimeValue>,
    state: &mut NodeMatchState,
) -> Result<bool, EvalSignal> {
    if fields.contains_key("identifier") || fields.contains_key("bind_identifier") {
        let ExprSpec::Name(name) = spec else {
            return Ok(false);
        };
        if node_match_string_field(fields, "identifier")?
            .is_some_and(|expected| expected != name.identifier)
        {
            return Ok(false);
        }
        if let Some(bind) = node_match_string_field(fields, "bind_identifier")? {
            if !state.bind(bind, string(&name.identifier))? {
                return Ok(false);
            }
        }
    }
    if fields.contains_key("value") || fields.contains_key("bind_value") {
        let ExprSpec::Literal(literal) = spec else {
            return Ok(false);
        };
        let runtime_literal = runtime_value_from_literal(&literal.value);
        if fields
            .get("value")
            .is_some_and(|expected| !runtime_values_equivalent(expected, &runtime_literal))
        {
            return Ok(false);
        }
        if let Some(bind) = node_match_string_field(fields, "bind_value")? {
            if !state.bind(bind, runtime_literal)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
