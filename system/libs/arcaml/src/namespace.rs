//! The namespace: a tree of named objects, built by loading the tables
//! and changed by the methods that run.

use crate::name::Seg;
use crate::value::{Holder, Value};
use crate::{NodeId, PciAddress};
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

#[derive(Clone, Debug)]
pub(crate) struct Method {
    /// Which loaded table holds the code, and where.
    pub table: u16,
    pub start: u32,
    pub end: u32,
    pub args: u8,
}

/// Methods the interpreter answers itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Builtin {
    /// `\_OSI("Windows 2015")`: does the system support this interface?
    Osi,
}

#[derive(Clone, Debug)]
pub(crate) struct Region {
    pub space: u8,
    pub base: u64,
    pub len: u64,
    /// For PCI configuration space: the function, once worked out.
    pub pci: Option<PciAddress>,
}

#[derive(Clone, Debug)]
pub(crate) enum FieldKind {
    Region(NodeId),
    /// A field reached after writing `value` to the `bank` field.
    Bank { region: NodeId, bank: NodeId, value: u64 },
    /// A field reached through an index and a data register.
    Index { index: NodeId, data: NodeId },
}

#[derive(Clone, Debug)]
pub(crate) struct Field {
    pub kind: FieldKind,
    pub bit_off: u64,
    pub bit_len: u64,
    /// AccessType: 0 any, 1 byte, 2 word, 3 dword, 4 qword, 5 buffer.
    pub access: u8,
    /// UpdateRule for the bits around the field: 0 preserve, 1 write as
    /// ones, 2 write as zeros.
    pub update: u8,
}

#[derive(Clone, Debug)]
pub(crate) struct BufferField {
    pub holder: Holder,
    pub bit_off: u64,
    pub bit_len: u64,
    /// Made by CreateField, which always reads as a buffer (like Windows
    /// and ACPICA), not by CreateByteField and the others.
    pub any_size: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum Object {
    /// A scope with no object of its own (`\_SB`, `\_GPE`).
    Scope,
    Data(Value),
    Device,
    Processor,
    PowerResource,
    ThermalZone,
    Method(Method),
    Builtin(Builtin),
    Mutex,
    /// An event and how many times it was signalled.
    Event(u32),
    Region(Region),
    Field(Field),
    BufferField(BufferField),
    Alias(NodeId),
}

/// The kind of a named object, as [`crate::Interpreter::object_type`] gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectType {
    Scope,
    Uninitialized,
    Integer,
    String,
    Buffer,
    Package,
    Reference,
    Field,
    Device,
    Event,
    Method,
    Mutex,
    Region,
    PowerResource,
    Processor,
    ThermalZone,
    BufferField,
}

pub(crate) struct Node {
    pub name: Seg,
    pub parent: NodeId,
    pub object: Object,
    /// In the order they were made.
    pub children: Vec<NodeId>,
}

pub(crate) struct Namespace {
    nodes: Vec<Option<Node>>,
    index: BTreeMap<(u32, Seg), u32>,
    /// Slots of removed nodes, for reuse.
    free: Vec<u32>,
    live: usize,
}

impl Namespace {
    pub fn new() -> Namespace {
        let root = Node { name: *b"\\___", parent: NodeId::ROOT, object: Object::Scope, children: Vec::new() };
        Namespace { nodes: alloc::vec![Some(root)], index: BTreeMap::new(), free: Vec::new(), live: 1 }
    }

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.0 as usize)?.as_ref()
    }

    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.get_mut(id.0 as usize)?.as_mut()
    }

    pub fn child(&self, parent: NodeId, seg: Seg) -> Option<NodeId> {
        self.index.get(&(parent.0, seg)).map(|&i| NodeId(i))
    }

    pub fn add(&mut self, parent: NodeId, seg: Seg, object: Object) -> NodeId {
        let node = Node { name: seg, parent, object, children: Vec::new() };
        let id = match self.free.pop() {
            Some(i) => {
                self.nodes[i as usize] = Some(node);
                i
            }
            None => {
                self.nodes.push(Some(node));
                (self.nodes.len() - 1) as u32
            }
        };
        self.index.insert((parent.0, seg), id);
        if let Some(p) = self.get_mut(parent) {
            p.children.push(NodeId(id));
        }
        self.live += 1;
        NodeId(id)
    }

    /// Removes a node and everything under it.
    pub fn remove(&mut self, id: NodeId) {
        if id == NodeId::ROOT {
            return;
        }
        let Some(parent) = self.get(id).map(|n| n.parent) else { return };
        if let Some(p) = self.get_mut(parent) {
            p.children.retain(|&c| c != id);
        }
        self.remove_orphan(id);
    }

    fn remove_orphan(&mut self, id: NodeId) {
        if let Some(n) = self.nodes.get_mut(id.0 as usize).and_then(|n| n.take()) {
            self.live -= 1;
            self.index.remove(&(n.parent.0, n.name));
            self.free.push(id.0);
            for c in n.children {
                self.remove_orphan(c);
            }
        }
    }

    /// How many nodes there are.
    pub fn len(&self) -> usize {
        self.live
    }

    /// Every node, in no particular order.
    pub fn ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.iter().enumerate().filter(|(_, n)| n.is_some()).map(|(i, _)| NodeId(i as u32))
    }
}
