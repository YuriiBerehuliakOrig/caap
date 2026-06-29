/// Mutable IR graph storage.
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::error::{CaapError, CaapResult};
use crate::ir::{ExprSpec, Node, NodeId};
use crate::source::SourceSpan;

#[derive(Clone, Debug)]
pub struct IRGraph {
    pub root_id: NodeId,
    nodes: HashMap<NodeId, Node>,
    parents: HashMap<NodeId, Option<NodeId>>,
    source_spans: HashMap<NodeId, SourceSpan>,
    internal_nodes: HashSet<NodeId>,
    top_level_forms: Vec<NodeId>,
    top_level_set: HashSet<NodeId>,
    next_id: NodeId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IRGraphTemplate {
    pub root_id: NodeId,
    pub nodes: Vec<Node>,
    pub parents: Vec<(NodeId, Option<NodeId>)>,
    pub source_spans: Vec<(NodeId, SourceSpan)>,
    #[serde(default)]
    pub internal_nodes: Vec<NodeId>,
    pub top_level_forms: Vec<NodeId>,
    pub next_id: NodeId,
}

impl IRGraph {
    pub fn new() -> Self {
        Self {
            root_id: 0,
            nodes: HashMap::new(),
            parents: HashMap::new(),
            source_spans: HashMap::new(),
            internal_nodes: HashSet::new(),
            top_level_forms: Vec::new(),
            top_level_set: HashSet::new(),
            next_id: 0,
        }
    }

    pub fn allocate_id(&mut self) -> CaapResult<NodeId> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| CaapError::graph("IRGraph node id overflow"))?;
        Ok(id)
    }

    /// Insert a node with an optional parent.  Children must already be present.
    pub fn set_node(&mut self, node: Node, parent_id: Option<NodeId>) -> CaapResult<()> {
        let id = node.id();
        let next_id = if id >= self.next_id {
            id.checked_add(1)
                .ok_or_else(|| CaapError::graph("IRGraph node id overflow"))?
        } else {
            self.next_id
        };
        self.nodes.insert(id, node);
        self.parents.insert(id, parent_id);
        self.next_id = next_id;
        Ok(())
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub fn parent(&self, id: NodeId) -> Option<Option<NodeId>> {
        self.parents.get(&id).copied()
    }

    pub fn source_span(&self, id: NodeId) -> Option<&SourceSpan> {
        self.source_spans.get(&id)
    }

    pub fn set_source_span(&mut self, id: NodeId, span: SourceSpan) -> CaapResult<()> {
        if !self.contains(id) {
            return Err(CaapError::graph(format!(
                "cannot attach source span to missing node: {id}"
            )));
        }
        self.source_spans.insert(id, span);
        Ok(())
    }

    pub fn mark_internal_node(&mut self, id: NodeId) -> CaapResult<()> {
        let Some(node) = self.nodes.get(&id) else {
            return Err(CaapError::graph(format!(
                "cannot mark missing node internal: {id}"
            )));
        };
        if !matches!(node, Node::Name(_)) {
            return Err(CaapError::graph(format!(
                "internal marker can only annotate name nodes: {id}"
            )));
        }
        self.internal_nodes.insert(id);
        Ok(())
    }

    pub fn is_internal_node(&self, id: NodeId) -> bool {
        self.internal_nodes.contains(&id)
    }

    pub fn top_level_form_ids(&self) -> &[NodeId] {
        &self.top_level_forms
    }

    pub fn add_top_level_form(&mut self, id: NodeId) -> CaapResult<()> {
        self.require_detached_top_level_form(id)?;
        if self.top_level_set.insert(id) {
            if self.top_level_forms.is_empty() {
                self.root_id = id;
            }
            self.top_level_forms.push(id);
        }
        Ok(())
    }

    pub fn set_top_level_form_ids(&mut self, form_ids: Vec<NodeId>) -> CaapResult<()> {
        let mut seen = HashSet::new();
        for &id in &form_ids {
            self.require_detached_top_level_form(id)?;
            if !seen.insert(id) {
                return Err(CaapError::graph(format!(
                    "duplicate top-level form id: {id}"
                )));
            }
        }
        self.top_level_set = form_ids.iter().copied().collect();
        self.top_level_forms = form_ids;
        Ok(())
    }

    pub fn has_top_level_form(&self, id: NodeId) -> bool {
        self.top_level_set.contains(&id)
    }

    pub fn insert_top_level_before(&mut self, anchor: NodeId, new_id: NodeId) -> CaapResult<()> {
        self.require_new_top_level_form(new_id)?;
        let index = self.top_level_index(anchor)?;
        self.top_level_forms.insert(index, new_id);
        self.top_level_set.insert(new_id);
        Ok(())
    }

    pub fn insert_top_level_after(&mut self, anchor: NodeId, new_id: NodeId) -> CaapResult<()> {
        self.require_new_top_level_form(new_id)?;
        let index = self.top_level_index(anchor)?;
        self.top_level_forms.insert(index + 1, new_id);
        self.top_level_set.insert(new_id);
        Ok(())
    }

    pub fn replace_top_level_form(&mut self, old_id: NodeId, new_id: NodeId) -> CaapResult<()> {
        let index = self.top_level_index(old_id)?;
        if old_id == new_id {
            return Ok(());
        }
        self.require_new_top_level_form(new_id)?;
        self.top_level_forms[index] = new_id;
        self.top_level_set.remove(&old_id);
        self.top_level_set.insert(new_id);
        Ok(())
    }

    pub fn remove_top_level_form(&mut self, id: NodeId) -> bool {
        if let Some(index) = self.top_level_forms.iter().position(|&item| item == id) {
            self.top_level_forms.remove(index);
            self.top_level_set.remove(&id);
            true
        } else {
            false
        }
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(&id)
    }

    pub fn node_ids(&self) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = self.nodes.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn validate_integrity(&self) -> CaapResult<()> {
        if self.nodes.is_empty() {
            if !self.parents.is_empty() {
                return Err(CaapError::graph("empty IRGraph cannot have parent entries"));
            }
            if !self.top_level_forms.is_empty() || !self.top_level_set.is_empty() {
                return Err(CaapError::graph(
                    "empty IRGraph cannot have top-level forms",
                ));
            }
            if !self.source_spans.is_empty() {
                return Err(CaapError::graph("empty IRGraph cannot have source spans"));
            }
            if !self.internal_nodes.is_empty() {
                return Err(CaapError::graph("empty IRGraph cannot have internal nodes"));
            }
            return Ok(());
        }

        if !self.nodes.contains_key(&self.root_id) {
            return Err(CaapError::graph(format!(
                "IRGraph root node is missing: {}",
                self.root_id
            )));
        }

        let mut max_id = None;
        for (&id, node) in &self.nodes {
            if node.id() != id {
                return Err(CaapError::graph(format!(
                    "IRGraph node key {id} does not match node id {}",
                    node.id()
                )));
            }
            max_id = Some(max_id.map_or(id, |current: NodeId| current.max(id)));
            if let Node::Call(call) = node {
                validate_unique_call_children(call.callee, &call.args)?;
                if !self.nodes.contains_key(&call.callee) {
                    return Err(CaapError::graph(format!(
                        "call {} has missing callee {}",
                        call.id, call.callee
                    )));
                }
                for &arg in call.args.iter() {
                    if !self.nodes.contains_key(&arg) {
                        return Err(CaapError::graph(format!(
                            "call {} has missing argument {arg}",
                            call.id
                        )));
                    }
                }
            }
        }

        for (&id, &parent) in &self.parents {
            if !self.nodes.contains_key(&id) {
                return Err(CaapError::graph(format!(
                    "parent entry references missing node: {id}"
                )));
            }
            if let Some(parent_id) = parent {
                if !self.nodes.contains_key(&parent_id) {
                    return Err(CaapError::graph(format!(
                        "parent entry for {id} references missing node: {parent_id}"
                    )));
                }
            }
        }
        for &id in self.nodes.keys() {
            if !self.parents.contains_key(&id) {
                return Err(CaapError::graph(format!(
                    "missing parent entry for node: {id}"
                )));
            }
        }

        let mut top_level = HashSet::new();
        for &id in &self.top_level_forms {
            if !self.nodes.contains_key(&id) {
                return Err(CaapError::graph(format!(
                    "top-level form references missing node: {id}"
                )));
            }
            if !top_level.insert(id) {
                return Err(CaapError::graph(format!(
                    "duplicate top-level form id: {id}"
                )));
            }
        }
        if top_level != self.top_level_set {
            return Err(CaapError::graph(
                "IRGraph top-level form set does not match ordered top-level forms",
            ));
        }

        let parentless: HashSet<NodeId> = self
            .parents
            .iter()
            .filter_map(|(&id, &parent)| if parent.is_none() { Some(id) } else { None })
            .collect();
        let expected_roots: HashSet<NodeId> = if self.top_level_forms.is_empty() {
            HashSet::from([self.root_id])
        } else {
            top_level
        };
        if parentless != expected_roots {
            let mut expected: Vec<NodeId> = expected_roots.into_iter().collect();
            let mut actual: Vec<NodeId> = parentless.into_iter().collect();
            expected.sort_unstable();
            actual.sort_unstable();
            return Err(CaapError::graph(format!(
                "IRGraph parentless nodes mismatch: expected={expected:?}, got={actual:?}"
            )));
        }

        for (&id, &parent) in &self.parents {
            if let Some(parent_id) = parent {
                let Some(parent_node) = self.nodes.get(&parent_id) else {
                    return Err(CaapError::graph(format!(
                        "parent entry for {id} references missing node: {parent_id}"
                    )));
                };
                if !parent_node.contains_child(id) {
                    return Err(CaapError::graph(format!(
                        "IRGraph parent {parent_id} does not reference child {id}"
                    )));
                }
            }
        }

        let mut color: HashMap<NodeId, u8> = HashMap::with_capacity(self.nodes.len());
        for &start in self.nodes.keys() {
            if color.get(&start) == Some(&2) {
                continue;
            }
            let mut path = Vec::new();
            let mut current = Some(start);
            loop {
                match current {
                    None => {
                        for &id in &path {
                            color.insert(id, 2);
                        }
                        break;
                    }
                    Some(id) => match color.get(&id).copied().unwrap_or(0) {
                        2 => {
                            for &pid in &path {
                                color.insert(pid, 2);
                            }
                            break;
                        }
                        1 => {
                            return Err(CaapError::graph("IRGraph parent links must be acyclic"));
                        }
                        _ => {
                            color.insert(id, 1);
                            path.push(id);
                            current = self.parents.get(&id).copied().flatten();
                        }
                    },
                }
            }
        }

        for (&id, node) in &self.nodes {
            for child_id in node.children() {
                let actual_parent = self.parents.get(&child_id).copied().flatten();
                if actual_parent != Some(id) {
                    return Err(CaapError::graph(format!(
                        "IRGraph child {child_id} parent mismatch: expected {id}, got {actual_parent:?}"
                    )));
                }
            }
        }

        for &id in self.source_spans.keys() {
            if !self.nodes.contains_key(&id) {
                return Err(CaapError::graph(format!(
                    "source span references missing node: {id}"
                )));
            }
        }

        for &id in &self.internal_nodes {
            let Some(node) = self.nodes.get(&id) else {
                return Err(CaapError::graph(format!(
                    "internal marker references missing node: {id}"
                )));
            };
            if !matches!(node, Node::Name(_)) {
                return Err(CaapError::graph(format!(
                    "internal marker can only annotate name nodes: {id}"
                )));
            }
        }

        if let Some(max_id) = max_id {
            if self.next_id <= max_id {
                return Err(CaapError::graph(format!(
                    "IRGraph next_id {} must exceed max node id {max_id}",
                    self.next_id
                )));
            }
        }

        Ok(())
    }

    pub fn validate_call_children(&self, callee: NodeId, args: &[NodeId]) -> CaapResult<()> {
        validate_unique_call_children(callee, args)?;
        if !self.contains(callee) {
            return Err(CaapError::graph(format!(
                "call callee node does not exist: {callee}"
            )));
        }
        for &arg in args {
            if !self.contains(arg) {
                return Err(CaapError::graph(format!(
                    "call argument node does not exist: {arg}"
                )));
            }
        }
        Ok(())
    }

    pub fn replace_node(&mut self, node_id: NodeId, node: Node) -> CaapResult<()> {
        self.require_node(node_id, "replacement target")?;
        if node.id() != node_id {
            return Err(CaapError::graph(
                "replacement node id must match replaced graph node id",
            ));
        }
        self.validate_node_children(&node)?;
        self.nodes.insert(node_id, node);
        Ok(())
    }

    pub fn delete_node(&mut self, node_id: NodeId) -> CaapResult<bool> {
        let Some(node) = self.nodes.get(&node_id) else {
            return Ok(false);
        };
        if self.parents.get(&node_id).copied().flatten().is_some() {
            return Err(CaapError::graph(
                "cannot delete attached IR graph node directly",
            ));
        }
        if node.has_children() {
            return Err(CaapError::graph(
                "cannot delete IR graph node with children directly",
            ));
        }
        self.remove_node_storage(node_id);
        Ok(true)
    }

    fn remove_node_storage(&mut self, node_id: NodeId) -> bool {
        let existed = self.nodes.remove(&node_id).is_some();
        self.parents.remove(&node_id);
        self.source_spans.remove(&node_id);
        self.internal_nodes.remove(&node_id);
        self.remove_top_level_form(node_id);
        if self.root_id == node_id {
            self.root_id = self
                .top_level_forms
                .first()
                .copied()
                .or_else(|| self.nodes.keys().min().copied())
                .unwrap_or(0);
        }
        existed
    }

    fn remove_subtree_storage(&mut self, root_id: NodeId) -> Vec<NodeId> {
        let mut dropped = Vec::new();
        let mut stack = vec![root_id];
        while let Some(current) = stack.pop() {
            if let Some(node) = self.nodes.get(&current) {
                stack.extend(node.children());
            }
            if self.remove_node_storage(current) {
                dropped.push(current);
            }
        }
        dropped
    }

    pub fn erase_detached_subtree(&mut self, root_id: NodeId) -> CaapResult<Vec<NodeId>> {
        self.require_node(root_id, "subtree root")?;
        if self.parents.get(&root_id).copied().flatten().is_some() {
            return Err(CaapError::graph(
                "cannot erase attached subtree root directly",
            ));
        }
        Ok(self.remove_subtree_storage(root_id))
    }

    pub fn replace_subtree(&mut self, old_id: NodeId, new_id: NodeId) -> CaapResult<Vec<NodeId>> {
        self.require_node(old_id, "subtree replacement target")?;
        if old_id == new_id {
            return Ok(Vec::new());
        }
        self.require_new_subtree_root(new_id)?;

        if self.has_top_level_form(old_id) {
            self.replace_top_level_form(old_id, new_id)?;
            if self.root_id == old_id {
                self.root_id = new_id;
            }
            return Ok(self.remove_subtree_storage(old_id));
        }

        let parent_id = self
            .parents
            .get(&old_id)
            .copied()
            .flatten()
            .ok_or_else(|| {
                CaapError::graph("cannot replace detached non-top-level subtree root")
            })?;
        let parent = self.nodes.get(&parent_id).cloned().ok_or_else(|| {
            CaapError::graph(format!("subtree parent does not exist: {parent_id}"))
        })?;
        let rewritten_parent = self.rewrite_parent_child(parent, old_id, new_id)?;

        self.parents.insert(new_id, Some(parent_id));
        self.replace_node(parent_id, rewritten_parent)?;
        Ok(self.remove_subtree_storage(old_id))
    }

    pub fn insert_expr_spec(&mut self, spec: &ExprSpec) -> CaapResult<NodeId> {
        insert_expr_spec_into(self, spec, None)
    }

    pub fn expr_spec_for_subtree(&self, root_id: NodeId) -> CaapResult<ExprSpec> {
        let node = self
            .node(root_id)
            .ok_or_else(|| CaapError::graph(format!("subtree root does not exist: {root_id}")))?;
        let span = self.source_span(root_id).cloned();
        match node {
            Node::Name(name) => Ok(ExprSpec::name_with_span(name.identifier.to_string(), span)?),
            Node::Literal(literal) => Ok(ExprSpec::literal_with_span(literal.value.clone(), span)),
            Node::Call(call) => {
                let callee = self.expr_spec_for_subtree(call.callee)?;
                let args = call
                    .args
                    .iter()
                    .map(|&arg| self.expr_spec_for_subtree(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(ExprSpec::call_with_span(callee, args, span))
            }
        }
    }

    fn validate_node_children(&self, node: &Node) -> CaapResult<()> {
        if let Node::Call(call) = node {
            validate_unique_call_children(call.callee, &call.args)?;
        }
        let children = node.children();
        for child_id in children {
            self.require_node(child_id, "child node")?;
            let actual_parent = self.parents.get(&child_id).copied().flatten();
            if actual_parent != Some(node.id()) {
                return Err(CaapError::graph(format!(
                    "IR graph child {child_id} parent mismatch: expected {}, got {actual_parent:?}",
                    node.id()
                )));
            }
        }
        Ok(())
    }

    fn rewrite_parent_child(
        &self,
        parent: Node,
        old_id: NodeId,
        new_id: NodeId,
    ) -> CaapResult<Node> {
        match parent {
            Node::Call(mut call) => {
                let mut replaced = false;
                if call.callee == old_id {
                    call.callee = new_id;
                    replaced = true;
                }
                for arg in std::rc::Rc::make_mut(&mut call.args) {
                    if *arg == old_id {
                        *arg = new_id;
                        replaced = true;
                    }
                }
                if replaced {
                    Ok(Node::Call(call))
                } else {
                    Err(CaapError::graph(format!(
                        "subtree parent {} does not reference child {old_id}",
                        call.id
                    )))
                }
            }
            Node::Name(_) | Node::Literal(_) => {
                Err(CaapError::graph("only call nodes can own child subtrees"))
            }
        }
    }

    fn require_node(&self, id: NodeId, label: &str) -> CaapResult<()> {
        if self.contains(id) {
            Ok(())
        } else {
            Err(CaapError::graph(format!("{label} does not exist: {id}")))
        }
    }

    fn require_detached_top_level_form(&self, id: NodeId) -> CaapResult<()> {
        self.require_node(id, "top-level form")?;
        if self.parents.get(&id).copied().flatten().is_some() {
            return Err(CaapError::graph(format!(
                "IR graph top-level form {id} has a parent"
            )));
        }
        Ok(())
    }

    fn require_new_subtree_root(&self, id: NodeId) -> CaapResult<()> {
        self.require_node(id, "replacement subtree root")?;
        if self.parents.get(&id).copied().flatten().is_some() {
            return Err(CaapError::graph(format!(
                "replacement subtree root {id} has a parent"
            )));
        }
        if self.has_top_level_form(id) {
            return Err(CaapError::graph(
                "replacement subtree root must not already be top-level",
            ));
        }
        Ok(())
    }

    fn require_new_top_level_form(&self, id: NodeId) -> CaapResult<()> {
        self.require_detached_top_level_form(id)?;
        if self.has_top_level_form(id) {
            return Err(CaapError::graph("IR graph top-level forms must be unique"));
        }
        Ok(())
    }

    fn top_level_index(&self, id: NodeId) -> CaapResult<usize> {
        self.top_level_forms
            .iter()
            .position(|&item| item == id)
            .ok_or_else(|| CaapError::graph(format!("top-level form does not exist: {id}")))
    }

    pub fn to_template(&self) -> IRGraphTemplate {
        // Sort by NodeId for a canonical, deterministic layout that can be
        // fingerprinted and compared across runs.
        let mut nodes: Vec<Node> = self.nodes.values().cloned().collect();
        nodes.sort_by_key(|n| n.id());

        let mut parents: Vec<(NodeId, Option<NodeId>)> = self
            .parents
            .iter()
            .map(|(&id, &parent)| (id, parent))
            .collect();
        parents.sort_by_key(|&(id, _)| id);

        let mut source_spans: Vec<(NodeId, SourceSpan)> = self
            .source_spans
            .iter()
            .map(|(&id, span)| (id, span.clone()))
            .collect();
        source_spans.sort_by_key(|&(id, _)| id);

        let mut internal_nodes: Vec<NodeId> = self.internal_nodes.iter().copied().collect();
        internal_nodes.sort_unstable();

        IRGraphTemplate {
            root_id: self.root_id,
            nodes,
            parents,
            source_spans,
            internal_nodes,
            top_level_forms: self.top_level_forms.clone(),
            next_id: self.next_id,
        }
    }

    pub fn from_template(template: IRGraphTemplate) -> CaapResult<Self> {
        template.validate()?;
        let nodes = template
            .nodes
            .into_iter()
            .map(|node| (node.id(), node))
            .collect();
        let parents = template.parents.into_iter().collect();
        let source_spans = template.source_spans.into_iter().collect();
        let internal_nodes = template.internal_nodes.into_iter().collect();
        let top_level_set = template.top_level_forms.iter().copied().collect();
        Ok(Self {
            root_id: template.root_id,
            nodes,
            parents,
            source_spans,
            internal_nodes,
            top_level_set,
            top_level_forms: template.top_level_forms,
            next_id: template.next_id,
        })
    }
}

impl IRGraphTemplate {
    pub fn validate(&self) -> CaapResult<()> {
        let mut ids = HashSet::new();
        let mut max_id = None;
        for node in &self.nodes {
            let id = node.id();
            if !ids.insert(id) {
                return Err(CaapError::graph(format!(
                    "IRGraphTemplate has duplicate node id: {id}"
                )));
            }
            max_id = Some(max_id.map_or(id, |current: NodeId| current.max(id)));
        }
        let node_by_id: HashMap<NodeId, &Node> =
            self.nodes.iter().map(|node| (node.id(), node)).collect();

        if self.nodes.is_empty() {
            if !self.parents.is_empty() {
                return Err(CaapError::graph(
                    "empty IRGraphTemplate cannot have parent entries",
                ));
            }
            if !self.top_level_forms.is_empty() {
                return Err(CaapError::graph(
                    "empty IRGraphTemplate cannot have top-level forms",
                ));
            }
            if !self.source_spans.is_empty() {
                return Err(CaapError::graph(
                    "empty IRGraphTemplate cannot have source spans",
                ));
            }
            if !self.internal_nodes.is_empty() {
                return Err(CaapError::graph(
                    "empty IRGraphTemplate cannot have internal nodes",
                ));
            }
            return Ok(());
        }

        if !ids.contains(&self.root_id) {
            return Err(CaapError::graph(format!(
                "IRGraphTemplate root node is missing: {}",
                self.root_id
            )));
        }

        for node in &self.nodes {
            if let Node::Call(call) = node {
                validate_unique_call_children(call.callee, &call.args)?;
                if !ids.contains(&call.callee) {
                    return Err(CaapError::graph(format!(
                        "call {} has missing callee {}",
                        call.id, call.callee
                    )));
                }
                for &arg in call.args.iter() {
                    if !ids.contains(&arg) {
                        return Err(CaapError::graph(format!(
                            "call {} has missing argument {arg}",
                            call.id
                        )));
                    }
                }
            }
        }

        let mut parent_ids = HashSet::new();
        for &(id, parent) in &self.parents {
            if !ids.contains(&id) {
                return Err(CaapError::graph(format!(
                    "parent entry references missing node: {id}"
                )));
            }
            if !parent_ids.insert(id) {
                return Err(CaapError::graph(format!(
                    "duplicate parent entry for node: {id}"
                )));
            }
            if let Some(parent_id) = parent {
                if !ids.contains(&parent_id) {
                    return Err(CaapError::graph(format!(
                        "parent entry for {id} references missing node: {parent_id}"
                    )));
                }
            }
        }
        for &id in &ids {
            if !parent_ids.contains(&id) {
                return Err(CaapError::graph(format!(
                    "missing parent entry for node: {id}"
                )));
            }
        }
        let parent_by_id: HashMap<NodeId, Option<NodeId>> = self.parents.iter().copied().collect();

        let mut top_level = HashSet::new();
        for &id in &self.top_level_forms {
            if !ids.contains(&id) {
                return Err(CaapError::graph(format!(
                    "top-level form references missing node: {id}"
                )));
            }
            if !top_level.insert(id) {
                return Err(CaapError::graph(format!(
                    "duplicate top-level form id: {id}"
                )));
            }
        }

        let parentless: HashSet<NodeId> = self
            .parents
            .iter()
            .filter_map(|&(id, parent)| if parent.is_none() { Some(id) } else { None })
            .collect();
        let expected_roots: HashSet<NodeId> = if self.top_level_forms.is_empty() {
            HashSet::from([self.root_id])
        } else {
            top_level.clone()
        };
        if parentless != expected_roots {
            let mut expected: Vec<NodeId> = expected_roots.into_iter().collect();
            let mut actual: Vec<NodeId> = parentless.into_iter().collect();
            expected.sort_unstable();
            actual.sort_unstable();
            return Err(CaapError::graph(format!(
                "IRGraphTemplate parentless nodes mismatch: expected={expected:?}, got={actual:?}"
            )));
        }

        for &(id, parent) in &self.parents {
            if let Some(parent_id) = parent {
                let Some(parent_node) = node_by_id.get(&parent_id) else {
                    return Err(CaapError::graph(format!(
                        "parent entry for {id} references missing node: {parent_id}"
                    )));
                };
                if !parent_node.contains_child(id) {
                    return Err(CaapError::graph(format!(
                        "IRGraphTemplate parent {parent_id} does not reference child {id}"
                    )));
                }
            }
        }

        // O(n) cycle detection with three-color marking.
        // 0 = unvisited, 1 = on active path, 2 = confirmed acyclic.
        let mut color: HashMap<NodeId, u8> = HashMap::with_capacity(ids.len());
        for &start in &ids {
            if color.get(&start) == Some(&2) {
                continue;
            }
            let mut path: Vec<NodeId> = Vec::new();
            let mut current = Some(start);
            loop {
                match current {
                    None => {
                        for &id in &path {
                            color.insert(id, 2);
                        }
                        break;
                    }
                    Some(id) => match color.get(&id).copied().unwrap_or(0) {
                        2 => {
                            for &pid in &path {
                                color.insert(pid, 2);
                            }
                            break;
                        }
                        1 => {
                            return Err(CaapError::graph(
                                "IRGraphTemplate parent links must be acyclic",
                            ));
                        }
                        _ => {
                            color.insert(id, 1);
                            path.push(id);
                            current = parent_by_id.get(&id).copied().flatten();
                        }
                    },
                }
            }
        }

        for node in &self.nodes {
            for child_id in node.children() {
                let actual_parent = parent_by_id.get(&child_id).copied().flatten();
                if actual_parent != Some(node.id()) {
                    return Err(CaapError::graph(format!(
                        "IRGraphTemplate child {child_id} parent mismatch: expected {}, got {actual_parent:?}",
                        node.id()
                    )));
                }
            }
        }

        let mut span_ids = HashSet::new();
        for &(id, _) in &self.source_spans {
            if !ids.contains(&id) {
                return Err(CaapError::graph(format!(
                    "source span references missing node: {id}"
                )));
            }
            if !span_ids.insert(id) {
                return Err(CaapError::graph(format!(
                    "duplicate source span for node: {id}"
                )));
            }
        }

        let mut internal_ids = HashSet::new();
        for &id in &self.internal_nodes {
            let Some(node) = node_by_id.get(&id) else {
                return Err(CaapError::graph(format!(
                    "internal marker references missing node: {id}"
                )));
            };
            if !matches!(node, Node::Name(_)) {
                return Err(CaapError::graph(format!(
                    "internal marker can only annotate name nodes: {id}"
                )));
            }
            if !internal_ids.insert(id) {
                return Err(CaapError::graph(format!(
                    "duplicate internal marker for node: {id}"
                )));
            }
        }

        if let Some(max_id) = max_id {
            if self.next_id <= max_id {
                return Err(CaapError::graph(format!(
                    "IRGraphTemplate next_id {} must exceed max node id {max_id}",
                    self.next_id
                )));
            }
        }

        Ok(())
    }
}

impl Default for IRGraph {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_unique_call_children(callee: NodeId, args: &[NodeId]) -> CaapResult<()> {
    let mut seen = HashSet::with_capacity(args.len() + 1);
    seen.insert(callee);
    for &arg in args {
        if !seen.insert(arg) {
            return Err(CaapError::graph(format!(
                "call child node appears more than once: {arg}"
            )));
        }
    }
    Ok(())
}

/// Convenience builder that allocates IDs automatically.
pub struct GraphBuilder {
    pub graph: IRGraph,
}

impl GraphBuilder {
    pub fn new() -> Self {
        Self {
            graph: IRGraph::new(),
        }
    }

    pub fn try_name(&mut self, identifier: impl Into<String>) -> CaapResult<NodeId> {
        let identifier = identifier.into();
        if identifier.is_empty() {
            return Err(CaapError::graph("name node identifier must be non-empty"));
        }
        let id = self.graph.allocate_id()?;
        self.graph.set_node(
            crate::ir::Node::Name(crate::ir::NameNode::new(id, identifier)?),
            None,
        )?;
        Ok(id)
    }

    pub fn try_internal_name(&mut self, identifier: impl Into<String>) -> CaapResult<NodeId> {
        let id = self.try_name(identifier)?;
        self.graph.mark_internal_node(id)?;
        Ok(id)
    }

    pub fn try_literal(&mut self, value: crate::ir::IrLiteralData) -> CaapResult<NodeId> {
        let id = self.graph.allocate_id()?;
        self.graph.set_node(
            crate::ir::Node::Literal(crate::ir::LiteralNode::new(id, value)),
            None,
        )?;
        Ok(id)
    }

    pub fn try_call(&mut self, callee: NodeId, args: Vec<NodeId>) -> CaapResult<NodeId> {
        let id = self.graph.allocate_id()?;
        self.try_call_with_id(id, callee, args, None)?;
        Ok(id)
    }

    pub fn try_call_with_id(
        &mut self,
        id: NodeId,
        callee: NodeId,
        args: Vec<NodeId>,
        parent_id: Option<NodeId>,
    ) -> CaapResult<NodeId> {
        if self.graph.contains(id) {
            return Err(CaapError::graph(format!(
                "call node id is already present: {id}"
            )));
        }
        self.graph.validate_call_children(callee, &args)?;
        self.graph.parents.insert(callee, Some(id));
        for &arg in &args {
            self.graph.parents.insert(arg, Some(id));
        }
        self.graph.set_node(
            crate::ir::Node::Call(crate::ir::CallNode::new(id, callee, args)),
            parent_id,
        )?;
        Ok(id)
    }

    pub fn lower_spec(&mut self, spec: &ExprSpec) -> CaapResult<NodeId> {
        insert_expr_spec_into(&mut self.graph, spec, None)
    }
}

fn insert_expr_spec_into(
    graph: &mut IRGraph,
    spec: &ExprSpec,
    parent_id: Option<NodeId>,
) -> CaapResult<NodeId> {
    let id = match spec {
        ExprSpec::Name(name) => {
            let id = graph.allocate_id()?;
            graph.set_node(
                crate::ir::Node::Name(crate::ir::NameNode::new(id, name.identifier.clone())?),
                parent_id,
            )?;
            id
        }
        ExprSpec::Literal(literal) => {
            let id = graph.allocate_id()?;
            graph.set_node(
                crate::ir::Node::Literal(crate::ir::LiteralNode::new(id, literal.value.clone())),
                parent_id,
            )?;
            id
        }
        ExprSpec::Call(call) => {
            let id = graph.allocate_id()?;
            let callee = insert_expr_spec_into(graph, &call.callee, Some(id))?;
            let mut args = Vec::with_capacity(call.args.len());
            for arg in call.args.iter() {
                args.push(insert_expr_spec_into(graph, arg, Some(id))?);
            }
            graph.validate_call_children(callee, &args)?;
            graph.set_node(
                crate::ir::Node::Call(crate::ir::CallNode::new(id, callee, args)),
                parent_id,
            )?;
            id
        }
    };
    if let Some(span) = spec.span() {
        graph.set_source_span(id, span.clone())?;
    }
    Ok(id)
}

impl Default for GraphBuilder {
    fn default() -> Self {
        Self::new()
    }
}
