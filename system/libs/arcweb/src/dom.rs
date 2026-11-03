//! The document tree: nodes in one vector, addressed by index.

use alloc::string::String;
use alloc::vec::Vec;

pub type NodeId = usize;

#[derive(Clone, Debug)]
pub struct Element {
    /// Lower case.
    pub tag: String,
    /// Names in lower case, in source order.
    pub attrs: Vec<(String, String)>,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn has_class(&self, class: &str) -> bool {
        self.attr("class").is_some_and(|c| c.split_ascii_whitespace().any(|x| x == class))
    }
}

#[derive(Clone, Debug)]
pub enum Data {
    Document,
    Element(Element),
    Text(String),
}

#[derive(Clone, Debug)]
pub struct Node {
    pub data: Data,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

#[derive(Clone, Debug, Default)]
pub struct Dom {
    pub nodes: Vec<Node>,
}

impl Dom {
    pub const ROOT: NodeId = 0;

    pub fn new() -> Dom {
        Dom { nodes: alloc::vec![Node { data: Data::Document, parent: None, children: Vec::new() }] }
    }

    pub fn add(&mut self, parent: NodeId, data: Data) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node { data, parent: Some(parent), children: Vec::new() });
        self.nodes[parent].children.push(id);
        id
    }

    /// A node outside the tree (made by a script).
    pub fn create(&mut self, data: Data) -> NodeId {
        self.nodes.push(Node { data, parent: None, children: Vec::new() });
        self.nodes.len() - 1
    }

    /// Takes a node out of its parent.
    pub fn detach(&mut self, id: NodeId) {
        if let Some(p) = self.nodes[id].parent.take() {
            self.nodes[p].children.retain(|&c| c != id);
        }
    }

    /// Whether `a` is `b` or one of its ancestors.
    pub fn contains(&self, a: NodeId, b: NodeId) -> bool {
        let mut cur = Some(b);
        while let Some(n) = cur {
            if n == a {
                return true;
            }
            cur = self.nodes[n].parent;
        }
        false
    }

    /// Puts `child` under `parent`, before `before` (or last). Refuses to
    /// put a node inside itself.
    pub fn insert(&mut self, parent: NodeId, child: NodeId, before: Option<NodeId>) -> bool {
        if self.contains(child, parent) {
            return false;
        }
        self.detach(child);
        let at = before.and_then(|b| self.nodes[parent].children.iter().position(|&c| c == b)).unwrap_or(self.nodes[parent].children.len());
        self.nodes[parent].children.insert(at, child);
        self.nodes[child].parent = Some(parent);
        true
    }

    /// Whether a node is in the document.
    pub fn attached(&self, id: NodeId) -> bool {
        self.contains(Self::ROOT, id)
    }

    /// A copy of a node (and of everything under it when `deep`), outside
    /// the tree.
    pub fn clone_node(&mut self, id: NodeId, deep: bool) -> NodeId {
        let copy = self.create(self.nodes[id].data.clone());
        if deep {
            for c in self.nodes[id].children.clone() {
                let cc = self.clone_node(c, true);
                self.insert(copy, cc, None);
            }
        }
        copy
    }

    pub fn element(&self, id: NodeId) -> Option<&Element> {
        match &self.nodes.get(id)?.data {
            Data::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut Element> {
        match &mut self.nodes.get_mut(id)?.data {
            Data::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn tag(&self, id: NodeId) -> &str {
        self.element(id).map_or("", |e| e.tag.as_str())
    }

    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        self.element(id)?.attr(name)
    }

    pub fn set_attr(&mut self, id: NodeId, name: &str, value: &str) {
        if let Some(e) = self.element_mut(id) {
            match e.attrs.iter_mut().find(|(k, _)| k == name) {
                Some(a) => a.1 = value.into(),
                None => e.attrs.push((name.into(), value.into())),
            }
        }
    }

    pub fn remove_attr(&mut self, id: NodeId, name: &str) {
        if let Some(e) = self.element_mut(id) {
            e.attrs.retain(|(k, _)| k != name);
        }
    }

    /// The text of a node and everything under it.
    pub fn text(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        match &self.nodes[id].data {
            Data::Text(t) => out.push_str(t),
            _ => {
                for &c in &self.nodes[id].children {
                    self.collect_text(c, out);
                }
            }
        }
    }

    /// Every element under `from` (itself included), in document order.
    pub fn descendants(&self, from: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = alloc::vec![from];
        while let Some(n) = stack.pop() {
            if matches!(self.nodes[n].data, Data::Element(_)) {
                out.push(n);
            }
            for &c in self.nodes[n].children.iter().rev() {
                stack.push(c);
            }
        }
        out
    }

    /// The first element with this tag.
    pub fn find(&self, tag: &str) -> Option<NodeId> {
        self.descendants(Self::ROOT).into_iter().find(|&n| self.tag(n) == tag)
    }

    /// The nearest ancestor (or itself) with this tag.
    pub fn closest(&self, mut id: NodeId, tag: &str) -> Option<NodeId> {
        loop {
            if self.tag(id) == tag {
                return Some(id);
            }
            id = self.nodes[id].parent?;
        }
    }

    pub fn parent_element(&self, id: NodeId) -> Option<NodeId> {
        let p = self.nodes[id].parent?;
        self.element(p).map(|_| p)
    }

    /// The element children of a node.
    pub fn element_children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes[id].children.iter().copied().filter(|&c| matches!(self.nodes[c].data, Data::Element(_)))
    }
}
