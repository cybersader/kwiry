// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::extract::ContentRole;

use super::error::{HtmlError, HtmlStage};
use super::limits::{Budget, HtmlLimits};
use super::tokenizer::Tag;

pub(super) type NodeId = u32;

#[derive(Debug)]
pub(super) enum NodeKind {
    Root,
    Element(ElementNode),
    Text(TextNode),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ElementNode {
    pub tag: Tag,
    pub role: ContentRole,
    pub fostered: bool,
}

#[derive(Debug)]
pub(super) struct TextNode {
    pub text: String,
    pub role: ContentRole,
    pub synthetic: bool,
}

#[derive(Debug)]
pub(super) struct Node {
    pub kind: NodeKind,
    pub parent: Option<NodeId>,
    pub first_child: Option<NodeId>,
    pub last_child: Option<NodeId>,
    pub previous_sibling: Option<NodeId>,
    pub next_sibling: Option<NodeId>,
}

#[derive(Debug)]
pub(super) struct Arena {
    nodes: Vec<Node>,
}

impl Arena {
    pub fn new(budget: &mut Budget, limits: &HtmlLimits) -> Result<Self, HtmlError> {
        budget.node(limits)?;
        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(1)
            .map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
        nodes.push(Node {
            kind: NodeKind::Root,
            parent: None,
            first_child: None,
            last_child: None,
            previous_sibling: None,
            next_sibling: None,
        });
        Ok(Self { nodes })
    }

    pub const fn root(&self) -> NodeId {
        0
    }

    pub fn get(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn get_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id as usize]
    }

    pub fn push(
        &mut self,
        kind: NodeKind,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<NodeId, HtmlError> {
        budget.node(limits)?;
        if self.nodes.len() == self.nodes.capacity() {
            self.nodes
                .try_reserve_exact(1)
                .map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
        }
        let id =
            u32::try_from(self.nodes.len()).map_err(|_| HtmlError::limit(HtmlStage::Recover))?;
        self.nodes.push(Node {
            kind,
            parent: None,
            first_child: None,
            last_child: None,
            previous_sibling: None,
            next_sibling: None,
        });
        Ok(id)
    }

    pub fn append_child(
        &mut self,
        parent: NodeId,
        child: NodeId,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        budget.mutation(limits, 1)?;
        let previous = self.get(parent).last_child;
        {
            let child_node = self.get_mut(child);
            child_node.parent = Some(parent);
            child_node.previous_sibling = previous;
            child_node.next_sibling = None;
        }
        if let Some(previous) = previous {
            self.get_mut(previous).next_sibling = Some(child);
        } else {
            self.get_mut(parent).first_child = Some(child);
        }
        self.get_mut(parent).last_child = Some(child);
        Ok(())
    }

    pub fn insert_before(
        &mut self,
        parent: NodeId,
        before: NodeId,
        child: NodeId,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        budget.mutation(limits, 1)?;
        let previous = self.get(before).previous_sibling;
        {
            let child_node = self.get_mut(child);
            child_node.parent = Some(parent);
            child_node.previous_sibling = previous;
            child_node.next_sibling = Some(before);
        }
        self.get_mut(before).previous_sibling = Some(child);
        if let Some(previous) = previous {
            self.get_mut(previous).next_sibling = Some(child);
        } else {
            self.get_mut(parent).first_child = Some(child);
        }
        Ok(())
    }

    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        self.get(id).first_child
    }

    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        self.get(id).last_child
    }

    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.get(id).next_sibling
    }

    pub fn previous_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.get(id).previous_sibling
    }
}

#[derive(Debug)]
pub(super) struct RecoveredDocument {
    pub arena: Arena,
    pub title: Option<String>,
    pub description: Option<String>,
    pub recovered_errors: usize,
}
