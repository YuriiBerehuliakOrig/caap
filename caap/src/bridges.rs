//! Shared runtime host-object bridges used by multiple builtin modules.

use std::rc::Rc;

use crate::error::{CaapError, CaapResult};
use crate::ir::NodeId;
use crate::semantic::{CapabilityName, SemanticEntry};
use crate::values::HostObject;

#[derive(Debug)]
pub struct NodeBridgeValue {
    pub(crate) unit: Rc<dyn HostObject>,
    pub(crate) node_id: NodeId,
}

impl NodeBridgeValue {
    pub(crate) fn new(unit: Rc<dyn HostObject>, node_id: NodeId) -> Self {
        Self { unit, node_id }
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }
}

impl HostObject for NodeBridgeValue {
    fn type_name(&self) -> &'static str {
        "node"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Clone, Debug)]
pub struct SemanticEntryBridgeValue {
    entry: SemanticEntry,
}

impl SemanticEntryBridgeValue {
    pub fn new(entry: SemanticEntry) -> Self {
        Self { entry }
    }

    pub fn entry(&self) -> &SemanticEntry {
        &self.entry
    }
}

impl HostObject for SemanticEntryBridgeValue {
    fn type_name(&self) -> &'static str {
        "semantic_entry"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostCapabilityBridgeValue {
    capability_kind: CapabilityName,
}

impl HostCapabilityBridgeValue {
    pub fn new(capability_kind: impl Into<String>) -> CaapResult<Self> {
        let capability_kind = capability_kind.into();
        let capability_kind = CapabilityName::new(&capability_kind).map_err(|error| {
            CaapError::host(format!("host capability kind is invalid: {error}"))
        })?;
        Ok(Self { capability_kind })
    }

    pub fn capability_kind(&self) -> &str {
        self.capability_kind.as_str()
    }
}

impl HostObject for HostCapabilityBridgeValue {
    fn type_name(&self) -> &'static str {
        "host_capability"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct DummyUnit;

    impl HostObject for DummyUnit {
        fn type_name(&self) -> &'static str {
            "dummy_unit"
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[test]
    fn node_bridge_preserves_host_unit_and_node_id() {
        let unit: Rc<dyn HostObject> = Rc::new(DummyUnit);
        let bridge = NodeBridgeValue::new(Rc::clone(&unit), 42);
        assert_eq!(bridge.node_id(), 42);
        assert_eq!(bridge.type_name(), "node");
        assert_eq!(bridge.unit.type_name(), "dummy_unit");
    }

    #[test]
    fn host_capability_rejects_empty_kind() {
        assert!(HostCapabilityBridgeValue::new("").is_err());
        assert_eq!(
            HostCapabilityBridgeValue::new("sys")
                .unwrap()
                .capability_kind(),
            "sys"
        );
    }
}
