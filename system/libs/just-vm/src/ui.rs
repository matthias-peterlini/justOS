//! Declarative UI tree produced by evaluating an JustScript `view`.
//!
//! The VM rebuilds the tree from scratch after every event, so rendering
//! back-ends are stateless: they draw a `UiNode` tree and report the
//! [`UiEvent`] (if any) produced by the user.

use crate::prelude::*;
pub use justc_core::ui::{Attr, AttrValue, Color, ColorToken, UiKind};
use core::fmt::Write as _;

/// Identifier of an element's event handler, valid until the next re-render.
pub type ActionId = usize;

/// A value produced or displayed by an interactive element.
#[derive(Clone, Debug, PartialEq)]
pub enum UiValue {
    Text(String),
    Bool(bool),
    Int(i64),
    Float(f64),
}

impl UiValue {
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            UiValue::Int(i) => Some(i as f64),
            UiValue::Float(f) => Some(f),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            UiValue::Bool(b) => Some(b),
            _ => None,
        }
    }
}

/// Something the user did, to be passed to [`Vm::dispatch`](crate::Vm::dispatch).
#[derive(Clone, Debug, PartialEq)]
pub enum UiEvent {
    /// A button was clicked.
    Click(ActionId),
    /// An input, checkbox, switch or slider produced a new value.
    Change(ActionId, UiValue),
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiNode {
    pub kind: UiKind,
    /// Display text: label and button text, window title, image path, input contents.
    pub text: String,
    /// Typed argument of interactive and numeric elements.
    pub value: Option<UiValue>,
    pub action: Option<ActionId>,
    /// Checked attributes; each attribute appears at most once.
    pub attrs: Vec<(Attr, AttrValue)>,
    pub children: Vec<UiNode>,
}

impl UiNode {
    pub fn new(kind: UiKind, text: String) -> Self {
        UiNode { kind, text, value: None, action: None, attrs: Vec::new(), children: Vec::new() }
    }

    pub fn attr(&self, attr: Attr) -> Option<&AttrValue> {
        self.attrs.iter().find(|(a, _)| *a == attr).map(|(_, v)| v)
    }

    /// Set an attribute; a later assignment replaces an earlier one.
    pub fn set_attr(&mut self, attr: Attr, value: AttrValue) {
        match self.attrs.iter_mut().find(|(a, _)| *a == attr) {
            Some(slot) => slot.1 = value,
            None => self.attrs.push((attr, value)),
        }
    }

    pub fn number(&self, attr: Attr) -> Option<f32> {
        match self.attr(attr)? {
            AttrValue::Number(v) => Some(*v as f32),
            _ => None,
        }
    }

    pub fn flag(&self, attr: Attr) -> Option<bool> {
        match self.attr(attr)? {
            AttrValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn color(&self, attr: Attr) -> Option<Color> {
        match self.attr(attr)? {
            AttrValue::Color(c) => Some(*c),
            _ => None,
        }
    }

    pub fn text_attr(&self, attr: Attr) -> Option<&str> {
        match self.attr(attr)? {
            AttrValue::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn choice(&self, attr: Attr) -> Option<&'static str> {
        match self.attr(attr)? {
            AttrValue::Choice(c) => Some(c),
            _ => None,
        }
    }

    pub fn icon(&self) -> Option<char> {
        match self.attr(Attr::Icon)? {
            AttrValue::Icon(c) => Some(*c),
            _ => None,
        }
    }

    /// This node and all its descendants, depth first.
    pub fn iter(&self) -> impl Iterator<Item = &UiNode> {
        let mut stack = vec![self];
        core::iter::from_fn(move || {
            let node = stack.pop()?;
            stack.extend(node.children.iter().rev());
            Some(node)
        })
    }

    /// Title of the first window in a tree, used for native window titles.
    pub fn window_title(nodes: &[UiNode]) -> Option<&str> {
        nodes.iter().find(|n| n.kind == UiKind::Window).map(|n| n.text.as_str()).filter(|t| !t.is_empty())
    }

    /// Plain-text rendering (console fallback and tests).
    pub fn render_text(nodes: &[UiNode]) -> String {
        fn go(n: &UiNode, depth: usize, out: &mut String) {
            let pad = "  ".repeat(depth);
            let caption = n.text_attr(Attr::Text).map(|t| format!(" {t}")).unwrap_or_default();
            let icon = n.icon().map(|_| "◆ ").unwrap_or_default();
            let _ = match n.kind {
                UiKind::Window => writeln!(out, "{pad}== {} ==", n.text),
                UiKind::Label => writeln!(out, "{pad}{icon}{}", n.text),
                UiKind::Button => {
                    let mark = if n.flag(Attr::Selected) == Some(true) { "*" } else { "" };
                    writeln!(out, "{pad}[ {icon}{}{mark} ]", n.text)
                }
                UiKind::Image => writeln!(out, "{pad}<image {}>", n.text),
                UiKind::Input => {
                    let shown = match (n.flag(Attr::Password), n.text.is_empty()) {
                        (Some(true), _) => "•".repeat(n.text.chars().count()),
                        (_, true) => n.text_attr(Attr::Placeholder).unwrap_or_default().to_owned(),
                        _ => n.text.clone(),
                    };
                    writeln!(out, "{pad}[{shown}_]")
                }
                UiKind::Checkbox => {
                    let on = n.value.as_ref().and_then(UiValue::as_bool) == Some(true);
                    writeln!(out, "{pad}[{}]{caption}", if on { "x" } else { " " })
                }
                UiKind::Switch => {
                    let on = n.value.as_ref().and_then(UiValue::as_bool) == Some(true);
                    writeln!(out, "{pad}({}){caption}", if on { "on" } else { "off" })
                }
                UiKind::Slider => {
                    let v = n.value.as_ref().and_then(UiValue::as_f64).unwrap_or(0.0);
                    writeln!(out, "{pad}<{v}>{caption}")
                }
                UiKind::Progress => {
                    let v = n.value.as_ref().and_then(UiValue::as_f64).unwrap_or(0.0).clamp(0.0, 1.0);
                    let filled = (v * 10.0).round() as usize;
                    writeln!(out, "{pad}[{}{}] {:.0}%{caption}", "#".repeat(filled), "-".repeat(10 - filled), v * 100.0)
                }
                UiKind::Divider => writeln!(out, "{pad}----"),
                UiKind::Spacer => Ok(()),
                UiKind::Accordion => {
                    let open = n.flag(Attr::Open) == Some(true);
                    writeln!(out, "{pad}{} {icon}{}", if open { "v" } else { ">" }, n.text)
                }
                UiKind::Column | UiKind::Row | UiKind::Card | UiKind::Grid | UiKind::Scroll | UiKind::Group | UiKind::Split => {
                    writeln!(out, "{pad}{}", n.kind.name())
                }
            };
            // A closed accordion hides its content.
            if n.kind == UiKind::Accordion && n.flag(Attr::Open) != Some(true) {
                return;
            }
            for c in &n.children {
                go(c, depth + 1, out);
            }
        }
        let mut out = String::new();
        for n in nodes {
            go(n, 0, &mut out);
        }
        out
    }
}

/// Builder used while a view function executes.
#[derive(Default)]
pub(crate) struct UiBuilder {
    stack: Vec<UiNode>,
    pub(crate) roots: Vec<UiNode>,
}

impl UiBuilder {
    pub(crate) fn open(&mut self, node: UiNode) -> Result<(), String> {
        self.check_parent(node.kind)?;
        self.stack.push(node);
        Ok(())
    }

    pub(crate) fn close(&mut self) -> Result<(), String> {
        let node = self.stack.pop().ok_or("unbalanced UI element")?;
        self.attach(node);
        Ok(())
    }

    pub(crate) fn leaf(&mut self, node: UiNode) -> Result<(), String> {
        self.check_parent(node.kind)?;
        self.attach(node);
        Ok(())
    }

    /// The innermost open element.
    pub(crate) fn current_mut(&mut self) -> Option<&mut UiNode> {
        self.stack.last_mut()
    }

    fn check_parent(&self, kind: UiKind) -> Result<(), String> {
        match self.stack.last() {
            Some(parent) if !parent.kind.is_container() => {
                Err(format!("`{}` cannot contain child elements (tried to add a `{}`)", parent.kind.name(), kind.name()))
            }
            _ => Ok(()),
        }
    }

    fn attach(&mut self, node: UiNode) {
        match self.stack.last_mut() {
            Some(parent) => parent.children.push(node),
            None => self.roots.push(node),
        }
    }
}
