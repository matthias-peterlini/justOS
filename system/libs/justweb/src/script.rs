//! Scripts on a page: [`justjs`] with the page's tree, its events and
//! timers.
//!
//! The document is lent to the interpreter only while script runs
//! ([`Scripts::run`], [`Scripts::pump`]). Elements are host objects whose
//! properties are read from and written to the tree; lists of nodes are
//! arrays taken when asked for. Whatever a script changes marks the page
//! for styling and layout again, and what the browser should do (follow a
//! link, send a form) comes back from [`Scripts::take_hits`].

use crate::css::{self, Viewport};
use crate::document::{Document, Hit};
use crate::dom::{Data, Dom, Element, NodeId};
use crate::{html, url};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use justjs::builtins::{num_arg, str_arg};
use justjs::interp::SYM_ITERATOR;
use justjs::{arg, Ctl, Interp, Key, Kind, ObjRef, Prop, Slot, Value, R};

const NODE: u32 = 1;
const STYLE: u32 = 2;
const CLASSES: u32 = 3;
const DATASET: u32 = 4;
const LOCATION: u32 = 5;
/// The window, as an event target.
const WINDOW: NodeId = usize::MAX;
const VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];

/// Fetches an address for a script: (method, address, body) → (status,
/// content type, text, address after redirects).
pub type Fetcher = fn(&str, &str, Option<&str>) -> Result<(u16, String, String, String), String>;

struct Listener {
    node: NodeId,
    kind: String,
    f: Value,
    capture: bool,
    once: bool,
}

struct Host {
    doc: *mut Document,
    nodes: BTreeMap<NodeId, ObjRef>,
    listeners: Vec<Listener>,
    /// Handlers written in attributes (`onclick="…"`): source and function.
    handlers: BTreeMap<(NodeId, String), (String, Value)>,
    node_proto: ObjRef,
    style_proto: ObjRef,
    list_proto: ObjRef,
    location: ObjRef,
    event: Value,
    fetcher: Option<Fetcher>,
    hits: Vec<Hit>,
    dirty: bool,
    sheets_dirty: bool,
    ready: &'static str,
}

fn host(it: &mut Interp) -> &mut Host {
    it.host.as_mut().and_then(|h| h.downcast_mut::<Host>()).expect("page host")
}

/// The page being run.
#[allow(clippy::mut_from_ref)]
fn doc<'a>(it: &Interp) -> &'a mut Document {
    let h = it.host.as_ref().and_then(|h| h.downcast_ref::<Host>()).expect("page host");
    assert!(!h.doc.is_null(), "script ran without its page");
    // SAFETY: `Scripts::lend` points `doc` at a `&mut Document` it holds
    // for the whole run and clears it afterwards. Callers keep the
    // reference only for a step and never across a call back into script,
    // so no two of them are used at once.
    unsafe { &mut *h.doc }
}

fn report(it: &mut Interp, c: Ctl) -> R<()> {
    match c {
        Ctl::Throw(v) => {
            let m = it.describe_error(&v);
            it.log(format!("Uncaught {m}"));
            Ok(())
        }
        Ctl::Halt => Err(Ctl::Halt),
        _ => Ok(()),
    }
}

fn changed(it: &mut Interp, n: NodeId) {
    let d = doc(it);
    let sheet = |m: NodeId| matches!(d.dom.tag(m), "style" | "link");
    let touches = sheet(n) || d.dom.nodes.get(n).and_then(|x| x.parent).is_some_and(sheet) || d.dom.descendants(n).into_iter().any(sheet);
    let h = host(it);
    h.dirty = true;
    h.sheets_dirty |= touches;
}

// ---------------------------------------------------------------------------
// Nodes as values

fn wrap(it: &mut Interp, n: NodeId) -> Value {
    if n == WINDOW {
        return Value::Obj(it.global.clone());
    }
    if let Some(o) = host(it).nodes.get(&n) {
        return Value::Obj(o.clone());
    }
    let proto = host(it).node_proto.clone();
    let o = it.alloc(Some(proto), Kind::Host(NODE, n));
    host(it).nodes.insert(n, o.clone());
    Value::Obj(o)
}

fn wrap_opt(it: &mut Interp, n: Option<NodeId>) -> Value {
    n.map_or(Value::Null, |n| wrap(it, n))
}

fn list(it: &mut Interp, nodes: Vec<NodeId>) -> Value {
    let v: Vec<Value> = nodes.into_iter().map(|n| wrap(it, n)).collect();
    it.new_array(v)
}

fn node_of(it: &mut Interp, v: &Value) -> R<NodeId> {
    if let Value::Obj(o) = v {
        if let Kind::Host(NODE, n) = o.borrow().kind {
            return Ok(n);
        }
    }
    Err(it.type_error("Illegal invocation: not a node"))
}

/// A node, or the window for the global object.
fn target_of(it: &mut Interp, v: &Value) -> R<NodeId> {
    if let Value::Obj(o) = v {
        if o.same(&it.global) {
            return Ok(WINDOW);
        }
    }
    node_of(it, v)
}

fn helper(it: &mut Interp, tag: u32, n: NodeId) -> Value {
    let proto = match tag {
        STYLE => host(it).style_proto.clone(),
        CLASSES => host(it).list_proto.clone(),
        _ => it.p.object.clone(),
    };
    Value::Obj(it.alloc(Some(proto), Kind::Host(tag, n)))
}

/// Text or nodes from `append(…)` and friends.
fn nodes_from(it: &mut Interp, args: &[Value]) -> R<Vec<NodeId>> {
    let mut out = Vec::new();
    for a in args {
        match a {
            Value::Obj(o) if matches!(o.borrow().kind, Kind::Host(NODE, _)) => {
                let n = node_of(it, a)?;
                // A fragment hands over its children.
                if doc(it).dom.tag(n) == "#fragment" {
                    out.extend(doc(it).dom.nodes[n].children.clone());
                } else {
                    out.push(n);
                }
            }
            v => {
                let t = it.to_str(v)?;
                out.push(doc(it).dom.create(Data::Text(t.to_string())));
            }
        }
    }
    Ok(out)
}

fn insert_all(it: &mut Interp, parent: NodeId, nodes: Vec<NodeId>, before: Option<NodeId>) -> R<()> {
    for n in nodes {
        if !doc(it).dom.insert(parent, n, before) {
            return Err(it.type_error("HierarchyRequestError: the new child contains the parent"));
        }
        changed(it, n);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HTML out

fn escape_into(s: &str, attr: bool, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' if !attr => out.push_str("&lt;"),
            '>' if !attr => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            c => out.push(c),
        }
    }
}

fn serialize(dom: &Dom, n: NodeId, out: &mut String) {
    match &dom.nodes[n].data {
        Data::Text(t) => {
            if dom.nodes[n].parent.is_some_and(|p| matches!(dom.tag(p), "script" | "style")) {
                out.push_str(t);
            } else {
                escape_into(t, false, out);
            }
        }
        Data::Element(e) if e.tag == "#fragment" => inner_into(dom, n, out),
        Data::Element(e) => {
            out.push('<');
            out.push_str(&e.tag);
            for (k, v) in &e.attrs {
                if k.starts_with("data-just-") {
                    continue;
                }
                out.push(' ');
                out.push_str(k);
                out.push_str("=\"");
                escape_into(v, true, out);
                out.push('"');
            }
            out.push('>');
            if VOID.contains(&e.tag.as_str()) {
                return;
            }
            inner_into(dom, n, out);
            out.push_str("</");
            out.push_str(&e.tag);
            out.push('>');
        }
        Data::Document => inner_into(dom, n, out),
    }
}

fn inner_into(dom: &Dom, n: NodeId, out: &mut String) {
    for &c in &dom.nodes[n].children {
        serialize(dom, c, out);
    }
}

/// Puts parsed HTML under `parent`, before `before`.
fn parse_html(it: &mut Interp, parent: NodeId, before: Option<NodeId>, src: &str) -> R<()> {
    let d = doc(it);
    let tmp = d.dom.create(Data::Element(Element { tag: "#fragment".into(), attrs: Vec::new() }));
    html::parse_into(&mut d.dom, tmp, src);
    let kids = d.dom.nodes[tmp].children.clone();
    insert_all(it, parent, kids, before)
}

fn clear_children(it: &mut Interp, n: NodeId) {
    let d = doc(it);
    for c in core::mem::take(&mut d.dom.nodes[n].children) {
        d.dom.nodes[c].parent = None;
    }
    changed(it, n);
}

// ---------------------------------------------------------------------------
// Properties of nodes

/// `className` → `class`, `htmlFor` → `for`, `readOnly` → `readonly`.
fn attr_name(key: &str) -> Option<&'static str> {
    Some(match key {
        "id" => "id",
        "className" => "class",
        "title" => "title",
        "name" => "name",
        "placeholder" => "placeholder",
        "alt" => "alt",
        "rel" => "rel",
        "target" => "target",
        "lang" => "lang",
        "dir" => "dir",
        "role" => "role",
        "htmlFor" => "for",
        "min" => "min",
        "max" => "max",
        "step" => "step",
        "pattern" => "pattern",
        "autocomplete" => "autocomplete",
        "method" => "method",
        "enctype" => "enctype",
        "width" => "width",
        "height" => "height",
        "colSpan" => "colspan",
        "rowSpan" => "rowspan",
        "download" => "download",
        "accept" => "accept",
        "label" => "label",
        "content" => "content",
        "charset" => "charset",
        "inputMode" => "inputmode",
        "srcset" => "srcset",
        "sizes" => "sizes",
        "loading" => "loading",
        "slot" => "slot",
        _ => return None,
    })
}

fn flag_name(key: &str) -> Option<&'static str> {
    Some(match key {
        "checked" => "checked",
        "disabled" => "disabled",
        "hidden" => "hidden",
        "selected" => "selected",
        "required" => "required",
        "readOnly" => "readonly",
        "multiple" => "multiple",
        "autofocus" => "autofocus",
        "open" => "open",
        "defaultChecked" => "checked",
        "noValidate" => "novalidate",
        "async" => "async",
        "defer" => "defer",
        "inert" => "inert",
        _ => return None,
    })
}

fn sval(s: &str) -> Value {
    Value::str(s)
}

/// The box of a node in the layout.
fn rect_of(d: &Document, n: NodeId) -> Option<justgfx::Rect> {
    let l = d.layout.as_ref()?;
    let mut found: Option<justgfx::Rect> = None;
    for (r, t) in &l.targets {
        if t.node() == n && r.w > 0 {
            found = Some(match found {
                Some(f) => {
                    let x = f.x.min(r.x);
                    let y = f.y.min(r.y);
                    justgfx::Rect::new(x, y, f.right().max(r.right()) - x, f.bottom().max(r.bottom()) - y)
                }
                None => *r,
            });
        }
    }
    found
}

fn document_get(it: &mut Interp, key: &str) -> R<Option<Value>> {
    let d = doc(it);
    let v = match key {
        "body" => {
            let b = d.dom.find("body");
            wrap_opt(it, b)
        }
        "head" => {
            let h = d.dom.find("head");
            wrap_opt(it, h)
        }
        "documentElement" => {
            let h = d.dom.find("html");
            wrap_opt(it, h)
        }
        "title" => sval(&d.title),
        "readyState" => sval(host(it).ready),
        "URL" | "documentURI" => sval(&d.url),
        "baseURI" => sval(&d.base),
        "referrer" | "cookie" => sval(""),
        "domain" => sval(url::host(&d.url)),
        "characterSet" | "charset" | "inputEncoding" => sval("UTF-8"),
        "contentType" => sval("text/html"),
        "compatMode" => sval("CSS1Compat"),
        "visibilityState" => sval("visible"),
        "hidden" => Value::Bool(false),
        "activeElement" => {
            let f = d.focus.or_else(|| d.dom.find("body"));
            wrap_opt(it, f)
        }
        "forms" | "images" | "links" | "scripts" => {
            let tags: &[&str] = match key {
                "forms" => &["form"],
                "images" => &["img"],
                "scripts" => &["script"],
                _ => &["a", "area"],
            };
            let found: Vec<NodeId> = d.dom.descendants(Dom::ROOT).into_iter().filter(|&n| tags.contains(&d.dom.tag(n)) && (key != "links" || d.dom.attr(n, "href").is_some())).collect();
            list(it, found)
        }
        "defaultView" => Value::Obj(it.global.clone()),
        "location" => Value::Obj(host(it).location.clone()),
        _ => return Ok(None),
    };
    Ok(Some(v))
}

fn node_get(it: &mut Interp, n: NodeId, key: &Key) -> R<Option<Value>> {
    let Key::Str(k) = key else { return Ok(None) };
    let key: &str = k;
    if n == Dom::ROOT {
        if let Some(v) = document_get(it, key)? {
            return Ok(Some(v));
        }
    }
    let d = doc(it);
    let Some(node) = d.dom.nodes.get(n) else { return Ok(None) };
    let tag = d.dom.tag(n).to_string();
    let is_el = matches!(node.data, Data::Element(_));
    let v = match key {
        "nodeType" => Value::Num(match node.data {
            Data::Element(_) if tag == "#fragment" => 11.0,
            Data::Element(_) => 1.0,
            Data::Text(_) => 3.0,
            Data::Document => 9.0,
        }),
        "nodeName" | "tagName" | "localName" => match &node.data {
            Data::Element(_) if tag == "#fragment" => sval("#document-fragment"),
            Data::Element(_) if key == "localName" => sval(&tag),
            Data::Element(_) => sval(&tag.to_ascii_uppercase()),
            Data::Text(_) if key == "nodeName" => sval("#text"),
            Data::Document if key == "nodeName" => sval("#document"),
            _ => return Ok(None),
        },
        "textContent" | "innerText" | "outerText" => match &node.data {
            Data::Document => Value::Null,
            _ => sval(&d.dom.text(n)),
        },
        "nodeValue" | "data" | "wholeText" => match &node.data {
            Data::Text(t) => sval(t),
            _ if key == "nodeValue" => Value::Null,
            _ => return Ok(None),
        },
        "innerHTML" => {
            let mut s = String::new();
            inner_into(&d.dom, n, &mut s);
            sval(&s)
        }
        "outerHTML" => {
            let mut s = String::new();
            serialize(&d.dom, n, &mut s);
            sval(&s)
        }
        "parentNode" => {
            let p = node.parent;
            wrap_opt(it, p)
        }
        "parentElement" => {
            let p = node.parent.filter(|&p| d.dom.element(p).is_some());
            wrap_opt(it, p)
        }
        "children" => {
            let c: Vec<NodeId> = d.dom.element_children(n).collect();
            list(it, c)
        }
        "childNodes" => {
            let c = node.children.clone();
            list(it, c)
        }
        "firstChild" | "lastChild" => {
            let c = if key == "firstChild" { node.children.first() } else { node.children.last() }.copied();
            wrap_opt(it, c)
        }
        "firstElementChild" | "lastElementChild" => {
            let mut kids = d.dom.element_children(n);
            let c = if key == "firstElementChild" { kids.next() } else { kids.last() };
            wrap_opt(it, c)
        }
        "childElementCount" => Value::Num(d.dom.element_children(n).count() as f64),
        "nextSibling" | "previousSibling" | "nextElementSibling" | "previousElementSibling" => {
            let Some(p) = node.parent else { return Ok(Some(Value::Null)) };
            let sibs: Vec<NodeId> = if key.ends_with("ElementSibling") { d.dom.element_children(p).collect() } else { d.dom.nodes[p].children.clone() };
            let at = sibs.iter().position(|&c| c == n);
            let s = at.and_then(|i| if key.starts_with("next") { sibs.get(i + 1).copied() } else { i.checked_sub(1).map(|j| sibs[j]) });
            wrap_opt(it, s)
        }
        "isConnected" => Value::Bool(d.dom.attached(n)),
        "ownerDocument" => wrap(it, Dom::ROOT),
        "style" if is_el => helper(it, STYLE, n),
        "classList" if is_el => helper(it, CLASSES, n),
        "dataset" if is_el => helper(it, DATASET, n),
        "type" if is_el => sval(&d.dom.attr(n, "type").map(str::to_ascii_lowercase).unwrap_or_else(|| String::from(match tag.as_str() {
            "input" => "text",
            "button" => "submit",
            "select" => "select-one",
            "textarea" => "textarea",
            _ => "",
        }))),
        "href" | "src" | "action" if is_el => match d.dom.attr(n, key) {
            Some(v) => sval(&url::join(&d.base, v.trim())),
            None if key == "action" => sval(&d.url),
            None => sval(""),
        },
        "value" if matches!(tag.as_str(), "input" | "textarea") => sval(&d.value(n)),
        "value" if tag == "option" => sval(&d.dom.attr(n, "value").map(String::from).unwrap_or_else(|| d.dom.text(n).trim().to_string())),
        "value" if tag == "select" => {
            let opts: Vec<NodeId> = d.dom.descendants(n).into_iter().filter(|&o| d.dom.tag(o) == "option").collect();
            let (_, sel) = d.options(n);
            sval(&opts.get(sel).map(|&o| d.dom.attr(o, "value").map(String::from).unwrap_or_else(|| d.dom.text(o).trim().to_string())).unwrap_or_default())
        }
        "defaultValue" if is_el => sval(d.dom.attr(n, "value").unwrap_or("")),
        "selectedIndex" if tag == "select" => Value::Num(d.options(n).1 as f64),
        "options" if tag == "select" => {
            let opts: Vec<NodeId> = d.dom.descendants(n).into_iter().filter(|&o| d.dom.tag(o) == "option").collect();
            list(it, opts)
        }
        "elements" if tag == "form" => {
            let els: Vec<NodeId> = d.dom.descendants(n).into_iter().filter(|&e| matches!(d.dom.tag(e), "input" | "select" | "textarea" | "button")).collect();
            list(it, els)
        }
        "form" if is_el && tag != "form" => {
            let f = d.form_of(n);
            wrap_opt(it, f)
        }
        "length" if matches!(tag.as_str(), "select" | "form") => {
            let k = if tag == "select" { &["option"][..] } else { &["input", "select", "textarea", "button"][..] };
            Value::Num(d.dom.descendants(n).into_iter().filter(|&e| e != n && k.contains(&d.dom.tag(e))).count() as f64)
        }
        "offsetWidth" | "offsetHeight" | "clientWidth" | "clientHeight" | "scrollWidth" | "scrollHeight" | "offsetTop" | "offsetLeft" | "clientTop" | "clientLeft" if is_el => {
            let r = rect_of(d, n).unwrap_or(justgfx::Rect::new(0, 0, 0, 0));
            Value::Num(match key {
                "offsetWidth" | "clientWidth" | "scrollWidth" => r.w,
                "offsetHeight" | "clientHeight" | "scrollHeight" => r.h,
                "offsetTop" => r.y,
                "offsetLeft" => r.x,
                _ => 0,
            } as f64)
        }
        "scrollTop" | "scrollLeft" if is_el => Value::Num(0.0),
        "tabIndex" if is_el => Value::Num(d.dom.attr(n, "tabindex").and_then(|v| v.trim().parse().ok()).unwrap_or(if matches!(tag.as_str(), "a" | "input" | "button" | "select" | "textarea") { 0.0 } else { -1.0 })),
        "offsetParent" if is_el => {
            let b = d.dom.find("body");
            wrap_opt(it, b)
        }
        _ if is_el => {
            if let Some(a) = attr_name(key) {
                sval(d.dom.attr(n, a).unwrap_or(""))
            } else if let Some(a) = flag_name(key) {
                Value::Bool(d.dom.attr(n, a).is_some())
            } else {
                return Ok(None);
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(v))
}

fn node_set(it: &mut Interp, n: NodeId, key: &Key, v: &Value) -> R<bool> {
    let Key::Str(k) = key else { return Ok(false) };
    let key: &str = k;
    if n == Dom::ROOT {
        match key {
            "title" => {
                let t = it.to_str(v)?;
                let d = doc(it);
                d.title = t.to_string();
                if let Some(tn) = d.dom.find("title") {
                    for c in core::mem::take(&mut d.dom.nodes[tn].children) {
                        d.dom.nodes[c].parent = None;
                    }
                    d.dom.add(tn, Data::Text(t.to_string()));
                }
                return Ok(true);
            }
            "cookie" => return Ok(true),
            "location" => {
                let u = it.to_str(v)?;
                navigate(it, &u);
                return Ok(true);
            }
            _ => {}
        }
    }
    if key.starts_with("on") && key.len() > 2 {
        if v.callable() {
            listen(it, n, &key[2..]);
        }
        return Ok(false);
    }
    let d = doc(it);
    let Some(node) = d.dom.nodes.get(n) else { return Ok(false) };
    let tag = d.dom.tag(n).to_string();
    let is_el = matches!(node.data, Data::Element(_));
    match key {
        "textContent" | "innerText" if !matches!(node.data, Data::Document) => {
            let t = it.to_str(v)?;
            if let Data::Text(old) = &mut doc(it).dom.nodes[n].data {
                *old = t.to_string();
            } else {
                clear_children(it, n);
                if !t.is_empty() {
                    let d = doc(it);
                    d.dom.add(n, Data::Text(t.to_string()));
                }
            }
            changed(it, n);
        }
        "nodeValue" | "data" => {
            let t = it.to_str(v)?;
            if let Data::Text(old) = &mut doc(it).dom.nodes[n].data {
                *old = t.to_string();
                changed(it, n);
            }
        }
        "innerHTML" if is_el => {
            let s = it.to_str(v)?;
            clear_children(it, n);
            parse_html(it, n, None, &s)?;
        }
        "outerHTML" if is_el => {
            let s = it.to_str(v)?;
            let d = doc(it);
            if let Some(p) = d.dom.nodes[n].parent {
                let next = d.dom.nodes[p].children.iter().position(|&c| c == n).and_then(|i| d.dom.nodes[p].children.get(i + 1).copied());
                d.dom.detach(n);
                parse_html(it, p, next, &s)?;
            }
        }
        "value" if matches!(tag.as_str(), "input" | "textarea") => {
            let s = it.to_str(v)?;
            let d = doc(it);
            d.set_value(n, &s);
            d.caret = d.caret.min(s.chars().count());
            changed(it, n);
        }
        "value" if tag == "select" => {
            let s = it.to_str(v)?;
            let d = doc(it);
            let opts: Vec<NodeId> = d.dom.descendants(n).into_iter().filter(|&o| d.dom.tag(o) == "option").collect();
            if let Some(i) = opts.iter().position(|&o| d.dom.attr(o, "value").map(String::from).unwrap_or_else(|| d.dom.text(o).trim().to_string()) == *s) {
                choose(d, n, i);
            }
            changed(it, n);
        }
        "selectedIndex" if tag == "select" => {
            let i = it.to_number(v)?;
            if i >= 0.0 {
                choose(doc(it), n, i as usize);
                changed(it, n);
            }
        }
        "defaultValue" if is_el => {
            let s = it.to_str(v)?;
            doc(it).dom.set_attr(n, "value", &s);
            changed(it, n);
        }
        "href" | "src" | "action" | "type" if is_el => {
            let s = it.to_str(v)?;
            doc(it).dom.set_attr(n, key, &s);
            changed(it, n);
        }
        "style" if is_el => {
            let s = it.to_str(v)?;
            doc(it).dom.set_attr(n, "style", &s);
            changed(it, n);
        }
        "scrollTop" | "scrollLeft" => {}
        _ if is_el => {
            if let Some(a) = attr_name(key) {
                let s = it.to_str(v)?;
                doc(it).dom.set_attr(n, a, &s);
            } else if let Some(a) = flag_name(key) {
                let d = doc(it);
                if v.truthy() {
                    // One radio of a group at a time.
                    if a == "checked" && d.dom.attr(n, "type").is_some_and(|t| t.eq_ignore_ascii_case("radio")) {
                        let name = d.dom.attr(n, "name").unwrap_or("").to_string();
                        for e in d.dom.descendants(Dom::ROOT) {
                            if e != n && d.dom.tag(e) == "input" && d.dom.attr(e, "name") == Some(&name) {
                                d.dom.remove_attr(e, "checked");
                            }
                        }
                    }
                    d.dom.set_attr(n, a, "");
                } else {
                    d.dom.remove_attr(n, a);
                }
            } else {
                return Ok(false);
            }
            changed(it, n);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn choose(d: &mut Document, select: NodeId, index: usize) {
    let scripted = core::mem::replace(&mut d.scripted, false);
    d.choose(select, index);
    d.scripted = scripted;
}

// --- style, classList, dataset

fn css_name(key: &str) -> String {
    if key.starts_with("--") {
        return key.to_string();
    }
    let mut out = String::new();
    for c in key.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    if out.starts_with("webkit-") || out.starts_with("moz-") || out.starts_with("ms-") {
        out.insert(0, '-');
    }
    if out == "float" || out == "css-float" {
        out = String::from("float");
    }
    out
}

fn declarations(style: &str) -> Vec<(String, String)> {
    style
        .split(';')
        .filter_map(|d| {
            let (k, v) = d.split_once(':')?;
            let k = k.trim().to_ascii_lowercase();
            (!k.is_empty()).then(|| (k, v.trim().to_string()))
        })
        .collect()
}

fn style_get(it: &mut Interp, n: NodeId, key: &str) -> Value {
    let s = doc(it).dom.attr(n, "style").unwrap_or("").to_string();
    if key == "cssText" {
        return sval(&s);
    }
    if key == "length" {
        return Value::Num(declarations(&s).len() as f64);
    }
    let name = css_name(key);
    sval(&declarations(&s).into_iter().rev().find(|(k, _)| *k == name).map(|(_, v)| v).unwrap_or_default())
}

fn style_put(it: &mut Interp, n: NodeId, name: &str, value: &str) {
    let d = doc(it);
    let mut decls = declarations(d.dom.attr(n, "style").unwrap_or(""));
    decls.retain(|(k, _)| k != name);
    if !value.trim().is_empty() {
        decls.push((name.to_string(), value.trim().to_string()));
    }
    let text = decls.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("; ");
    if text.is_empty() {
        d.dom.remove_attr(n, "style");
    } else {
        d.dom.set_attr(n, "style", &text);
    }
    changed(it, n);
}

fn classes(it: &mut Interp, n: NodeId) -> Vec<String> {
    doc(it).dom.attr(n, "class").unwrap_or("").split_ascii_whitespace().map(String::from).collect()
}

fn set_classes(it: &mut Interp, n: NodeId, list: &[String]) {
    doc(it).dom.set_attr(n, "class", &list.join(" "));
    changed(it, n);
}

fn data_name(key: &str) -> String {
    let mut out = String::from("data-");
    for c in key.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Whether `key` is a method of `proto` or of `Object.prototype`.
fn inherited(it: &Interp, proto: &ObjRef, key: &Key) -> bool {
    proto.borrow().props.get(key).is_some() || it.p.object.borrow().props.get(key).is_some()
}

fn host_get(it: &mut Interp, _: &ObjRef, tag: u32, n: NodeId, key: &Key) -> R<Option<Value>> {
    match tag {
        NODE => node_get(it, n, key),
        STYLE => {
            let proto = host(it).style_proto.clone();
            match key {
                Key::Str(k) if !inherited(it, &proto, key) => Ok(Some(style_get(it, n, k))),
                _ => Ok(None),
            }
        }
        CLASSES => {
            let list = classes(it, n);
            Ok(match key {
                k if k.is("length") => Some(Value::Num(list.len() as f64)),
                k if k.is("value") => Some(sval(&list.join(" "))),
                k => k.index().map(|i| list.get(i).map_or(Value::Undef, |c| sval(c))),
            })
        }
        DATASET => Ok(match key {
            Key::Str(k) => doc(it).dom.attr(n, &data_name(k)).map(sval),
            Key::Sym(_) => None,
        }),
        LOCATION => {
            let Key::Str(k) = key else { return Ok(None) };
            let u = doc(it).url.clone();
            Ok(location_part(&u, k).map(|s| sval(&s)))
        }
        _ => Ok(None),
    }
}

fn host_set(it: &mut Interp, _: &ObjRef, tag: u32, n: NodeId, key: &Key, v: &Value) -> R<bool> {
    match tag {
        NODE => node_set(it, n, key, v),
        STYLE => {
            let Key::Str(k) = key else { return Ok(false) };
            let s = if v.nullish() { String::new() } else { it.to_str(v)?.to_string() };
            if &**k == "cssText" {
                doc(it).dom.set_attr(n, "style", &s);
                changed(it, n);
            } else {
                style_put(it, n, &css_name(k), &s);
            }
            Ok(true)
        }
        CLASSES => {
            if key.is("value") {
                let s = it.to_str(v)?;
                doc(it).dom.set_attr(n, "class", &s);
                changed(it, n);
                return Ok(true);
            }
            Ok(false)
        }
        DATASET => {
            let Key::Str(k) = key else { return Ok(false) };
            let s = it.to_str(v)?;
            doc(it).dom.set_attr(n, &data_name(k), &s);
            changed(it, n);
            Ok(true)
        }
        LOCATION => {
            if key.is("href") {
                let s = it.to_str(v)?;
                navigate(it, &s);
                return Ok(true);
            }
            if key.is("hash") {
                let s = it.to_str(v)?;
                let base = doc(it).url.split('#').next().unwrap_or("").to_string();
                let h = s.trim_start_matches('#');
                navigate(it, &format!("{base}#{h}"));
                return Ok(true);
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn location_part(u: &str, key: &str) -> Option<String> {
    let (scheme, rest) = u.split_once("://").unwrap_or(("", u));
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let hostport = &rest[..host_end];
    let path_q = &rest[host_end..];
    let (no_hash, hash) = match path_q.find('#') {
        Some(i) => (&path_q[..i], &path_q[i..]),
        None => (path_q, ""),
    };
    let (path, search) = match no_hash.find('?') {
        Some(i) => (&no_hash[..i], &no_hash[i..]),
        None => (no_hash, ""),
    };
    let (hostname, port) = hostport.split_once(':').unwrap_or((hostport, ""));
    Some(match key {
        "href" => u.to_string(),
        "protocol" => format!("{scheme}:"),
        "host" => hostport.to_string(),
        "hostname" => hostname.to_string(),
        "port" => port.to_string(),
        "pathname" => if path.is_empty() { String::from("/") } else { path.to_string() },
        "search" => if search.len() > 1 { search.to_string() } else { String::new() },
        "hash" => if hash.len() > 1 { hash.to_string() } else { String::new() },
        "origin" => format!("{scheme}://{hostport}"),
        _ => return None,
    })
}

fn navigate(it: &mut Interp, to: &str) {
    let d = doc(it);
    let u = url::join(&d.base, to.trim());
    if u.to_ascii_lowercase().starts_with("javascript:") {
        return;
    }
    host(it).hits.push(Hit::Open(u));
}

// ---------------------------------------------------------------------------
// Events

fn listen(it: &mut Interp, n: NodeId, kind: &str) {
    if matches!(kind, "click" | "mousedown" | "mouseup" | "pointerdown" | "pointerup" | "touchstart" | "touchend" | "submit") {
        doc(it).listened.insert(if n == WINDOW { Dom::ROOT } else { n });
    }
}

fn make_event(it: &mut Interp, kind: &str, bubbles: bool) -> R<Value> {
    let ctor = host(it).event.clone();
    let opts = it.new_obj();
    opts.borrow_mut().props.insert(Key::str("bubbles"), Prop::data(Value::Bool(bubbles)));
    opts.borrow_mut().props.insert(Key::str("cancelable"), Prop::data(Value::Bool(true)));
    it.construct(&ctor, &[sval(kind), Value::Obj(opts)], None)
}

/// Elements with an id are globals of the window.
fn named_element(it: &mut Interp, name: &str) -> Option<Value> {
    let d = doc(it);
    let found = d.dom.descendants(Dom::ROOT).into_iter().find(|&e| d.dom.attr(e, "id") == Some(name))?;
    Some(wrap(it, found))
}

fn data_of(p: &Prop) -> Option<Value> {
    match &p.slot {
        Slot::Data(v) => Some(v.clone()),
        _ => None,
    }
}

/// The handler an `on…` attribute or property holds.
fn handler(it: &mut Interp, n: NodeId, kind: &str) -> R<Value> {
    let prop = format!("on{kind}");
    let own = if n == WINDOW {
        it.global.borrow().props.get(&Key::str(&prop)).and_then(data_of)
    } else {
        host(it).nodes.get(&n).cloned().and_then(|o| o.borrow().props.get(&Key::str(&prop)).and_then(data_of))
    };
    if let Some(f) = own.filter(Value::callable) {
        return Ok(f);
    }
    // `<body onload>` is the window's.
    let d = doc(it);
    let el = if n == WINDOW { d.dom.find("body") } else { Some(n) };
    let Some(code) = el.and_then(|e| d.dom.attr(e, &prop)).map(String::from) else { return Ok(Value::Undef) };
    let cache_key = (n, kind.to_string());
    if let Some((src, f)) = host(it).handlers.get(&cache_key) {
        if *src == code {
            return Ok(f.clone());
        }
    }
    let f = match it.eval_src(&format!("(function (event) {{\n{code}\n}})")) {
        Ok(f) => f,
        Err(c) => {
            report(it, c)?;
            Value::Undef
        }
    };
    host(it).handlers.insert(cache_key, (code, f.clone()));
    Ok(f)
}

/// Calls the listeners of one node. Returns whether propagation stopped.
fn fire_at(it: &mut Interp, n: NodeId, kind: &str, ev: &Value, capture: Option<bool>) -> R<bool> {
    let this = wrap(it, n);
    it.put(ev, Key::str("currentTarget"), this.clone())?;
    if capture != Some(true) {
        let f = handler(it, n, kind)?;
        if f.callable() {
            match it.call(&f, this.clone(), core::slice::from_ref(ev)) {
                Ok(Value::Bool(false)) => it.put(ev, Key::str("defaultPrevented"), Value::Bool(true))?,
                Ok(_) => {}
                Err(c) => report(it, c)?,
            }
        }
    }
    let todo: Vec<(usize, Value, bool)> = host(it)
        .listeners
        .iter()
        .enumerate()
        .filter(|(_, l)| l.node == n && l.kind == kind && capture.is_none_or(|c| c == l.capture))
        .map(|(i, l)| (i, l.f.clone(), l.once))
        .collect();
    let mut gone = Vec::new();
    for (i, f, once) in todo {
        if once {
            gone.push(i);
        }
        let f = if f.callable() { f } else { it.get(&f, &Key::str("handleEvent"))? };
        if let Err(c) = it.call(&f, this.clone(), core::slice::from_ref(ev)) {
            report(it, c)?;
        }
        if it.get(ev, &Key::str("__now"))?.truthy() {
            break;
        }
    }
    let h = host(it);
    for i in gone.into_iter().rev() {
        if i < h.listeners.len() {
            h.listeners.remove(i);
        }
    }
    Ok(it.get(ev, &Key::str("__stop"))?.truthy())
}

/// Sends an event through the tree. Returns false if a listener cancelled
/// what it would do.
fn dispatch(it: &mut Interp, target: NodeId, ev: &Value) -> R<bool> {
    let kind = it.get(ev, &Key::str("type"))?;
    let kind = it.to_str(&kind)?.to_string();
    let bubbles = it.get(ev, &Key::str("bubbles"))?.truthy();
    let t = wrap(it, target);
    it.put(ev, Key::str("target"), t.clone())?;
    it.put(ev, Key::str("srcElement"), t)?;
    let mut path = vec![target];
    if target != WINDOW {
        let d = doc(it);
        let mut cur = d.dom.nodes.get(target).and_then(|x| x.parent);
        while let Some(p) = cur {
            path.push(p);
            cur = d.dom.nodes[p].parent;
        }
        if path.last() == Some(&Dom::ROOT) {
            path.push(WINDOW);
        }
    }
    let mut stopped = false;
    for &n in path[1..].iter().rev() {
        if fire_at(it, n, &kind, ev, Some(true))? {
            stopped = true;
            break;
        }
    }
    if !stopped {
        it.put(ev, Key::str("eventPhase"), Value::Num(2.0))?;
        stopped = fire_at(it, target, &kind, ev, None)?;
    }
    if !stopped && bubbles {
        it.put(ev, Key::str("eventPhase"), Value::Num(3.0))?;
        for &n in &path[1..] {
            if fire_at(it, n, &kind, ev, Some(false))? {
                break;
            }
        }
    }
    it.put(ev, Key::str("currentTarget"), Value::Null)?;
    Ok(!it.get(ev, &Key::str("defaultPrevented"))?.truthy())
}

fn fire(it: &mut Interp, target: NodeId, kind: &str, bubbles: bool) -> R<bool> {
    let ev = make_event(it, kind, bubbles)?;
    dispatch(it, target, &ev)
}

fn is_submit(d: &Document, n: NodeId) -> bool {
    match d.dom.tag(n) {
        "button" => d.dom.attr(n, "type").unwrap_or("submit").eq_ignore_ascii_case("submit"),
        "input" => d.dom.attr(n, "type").is_some_and(|t| t.eq_ignore_ascii_case("submit") || t.eq_ignore_ascii_case("image")),
        _ => false,
    }
}

/// A click from the user or from `el.click()`, with what it does unless
/// cancelled.
fn click(it: &mut Interp, n: NodeId, deferred: bool) -> R<()> {
    for k in ["pointerdown", "mousedown", "pointerup", "mouseup"] {
        fire(it, n, k, true)?;
    }
    if !fire(it, n, "click", true)? || !deferred {
        return Ok(());
    }
    let d = doc(it);
    if is_submit(d, n) {
        if let Some(form) = d.form_of(n) {
            if !fire(it, form, "submit", true)? {
                return Ok(());
            }
        }
    }
    let hit = doc(it).default_click(n);
    match hit {
        Some(Hit::Changed) => host(it).dirty = true,
        Some(h) => host(it).hits.push(h),
        None => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Methods

fn this_node(it: &mut Interp, this: &Value) -> R<NodeId> {
    node_of(it, this)
}

fn selector_nodes(it: &mut Interp, from: NodeId, sel: &str, first: bool) -> Vec<NodeId> {
    let list = css::parse_selector_list(sel);
    let d = doc(it);
    let mut out = Vec::new();
    if list.is_empty() {
        return out;
    }
    for e in d.dom.descendants(from) {
        if e != from && list.iter().any(|s| css::matches(&d.dom, e, s)) {
            out.push(e);
            if first {
                break;
            }
        }
    }
    out
}

fn install_node_methods(it: &mut Interp, p: &ObjRef) {
    it.method(p, "getAttribute", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let k = str_arg(it, args, 0)?.to_ascii_lowercase();
        Ok(doc(it).dom.attr(n, &k).map_or(Value::Null, sval))
    });
    it.method(p, "setAttribute", 2, |it, this, args, _| {
        let n = this_node(it, this)?;
        let k = str_arg(it, args, 0)?.to_ascii_lowercase();
        let v = str_arg(it, args, 1)?;
        doc(it).dom.set_attr(n, &k, &v);
        if let Some(kind) = k.strip_prefix("on") {
            listen(it, n, kind);
        }
        changed(it, n);
        Ok(Value::Undef)
    });
    it.method(p, "removeAttribute", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let k = str_arg(it, args, 0)?.to_ascii_lowercase();
        doc(it).dom.remove_attr(n, &k);
        changed(it, n);
        Ok(Value::Undef)
    });
    it.method(p, "hasAttribute", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let k = str_arg(it, args, 0)?.to_ascii_lowercase();
        Ok(Value::Bool(doc(it).dom.attr(n, &k).is_some()))
    });
    it.method(p, "toggleAttribute", 2, |it, this, args, _| {
        let n = this_node(it, this)?;
        let k = str_arg(it, args, 0)?.to_ascii_lowercase();
        let d = doc(it);
        let on = if args.len() > 1 { arg(args, 1).truthy() } else { d.dom.attr(n, &k).is_none() };
        if on {
            d.dom.set_attr(n, &k, "");
        } else {
            d.dom.remove_attr(n, &k);
        }
        changed(it, n);
        Ok(Value::Bool(on))
    });
    it.method(p, "getAttributeNames", 0, |it, this, _, _| {
        let n = this_node(it, this)?;
        let names: Vec<Value> = doc(it).dom.element(n).map(|e| e.attrs.iter().map(|(k, _)| sval(k)).collect()).unwrap_or_default();
        Ok(it.new_array(names))
    });
    it.method(p, "hasAttributes", 0, |it, this, _, _| {
        let n = this_node(it, this)?;
        Ok(Value::Bool(doc(it).dom.element(n).is_some_and(|e| !e.attrs.is_empty())))
    });
    it.method(p, "appendChild", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let c = arg(args, 0);
        let kids = nodes_from(it, core::slice::from_ref(&c))?;
        insert_all(it, n, kids, None)?;
        Ok(c)
    });
    it.method(p, "append", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let kids = nodes_from(it, args)?;
        insert_all(it, n, kids, None)?;
        Ok(Value::Undef)
    });
    it.method(p, "prepend", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let kids = nodes_from(it, args)?;
        let first = doc(it).dom.nodes[n].children.first().copied();
        insert_all(it, n, kids, first)?;
        Ok(Value::Undef)
    });
    it.method(p, "replaceChildren", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let kids = nodes_from(it, args)?;
        clear_children(it, n);
        insert_all(it, n, kids, None)?;
        Ok(Value::Undef)
    });
    it.method(p, "insertBefore", 2, |it, this, args, _| {
        let n = this_node(it, this)?;
        let c = arg(args, 0);
        let kids = nodes_from(it, core::slice::from_ref(&c))?;
        let before = match arg(args, 1) {
            Value::Null | Value::Undef => None,
            b => Some(node_of(it, &b)?),
        };
        insert_all(it, n, kids, before)?;
        Ok(c)
    });
    it.method(p, "removeChild", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let c = node_arg(it, args, 0)?;
        if doc(it).dom.nodes[c].parent != Some(n) {
            return Err(it.type_error("NotFoundError: the node is not a child of this node"));
        }
        changed(it, c);
        doc(it).dom.detach(c);
        Ok(arg(args, 0))
    });
    it.method(p, "replaceChild", 2, |it, this, args, _| {
        let n = this_node(it, this)?;
        let old = node_arg(it, args, 1)?;
        let kids = nodes_from(it, &args[..1.min(args.len())])?;
        insert_all(it, n, kids, Some(old))?;
        changed(it, old);
        doc(it).dom.detach(old);
        Ok(arg(args, 1))
    });
    it.method(p, "remove", 0, |it, this, _, _| {
        let n = this_node(it, this)?;
        changed(it, n);
        doc(it).dom.detach(n);
        Ok(Value::Undef)
    });
    fn sibling_insert(it: &mut Interp, this: &Value, args: &[Value], after: bool, replace: bool) -> R<Value> {
        let n = this_node(it, this)?;
        let Some(p) = doc(it).dom.nodes[n].parent else { return Ok(Value::Undef) };
        let kids = nodes_from(it, args)?;
        let d = doc(it);
        let before = if after {
            let sibs = &d.dom.nodes[p].children;
            sibs.iter().position(|&c| c == n).and_then(|i| sibs.get(i + 1).copied())
        } else {
            Some(n)
        };
        insert_all(it, p, kids, before)?;
        if replace {
            changed(it, n);
            doc(it).dom.detach(n);
        }
        Ok(Value::Undef)
    }
    it.method(p, "before", 1, |it, this, args, _| sibling_insert(it, this, args, false, false));
    it.method(p, "after", 1, |it, this, args, _| sibling_insert(it, this, args, true, false));
    it.method(p, "replaceWith", 1, |it, this, args, _| sibling_insert(it, this, args, true, true));
    it.method(p, "cloneNode", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let c = doc(it).dom.clone_node(n, arg(args, 0).truthy());
        Ok(wrap(it, c))
    });
    it.method(p, "contains", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let Value::Obj(_) = arg(args, 0) else { return Ok(Value::Bool(false)) };
        let c = node_arg(it, args, 0)?;
        Ok(Value::Bool(doc(it).dom.contains(n, c)))
    });
    it.method(p, "hasChildNodes", 0, |it, this, _, _| {
        let n = this_node(it, this)?;
        Ok(Value::Bool(!doc(it).dom.nodes[n].children.is_empty()))
    });
    it.method(p, "getRootNode", 0, |it, this, _, _| {
        let mut n = this_node(it, this)?;
        while let Some(p) = doc(it).dom.nodes[n].parent {
            n = p;
        }
        Ok(wrap(it, n))
    });
    it.method(p, "querySelector", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let s = str_arg(it, args, 0)?;
        let found = selector_nodes(it, n, &s, true).first().copied();
        Ok(wrap_opt(it, found))
    });
    it.method(p, "querySelectorAll", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let s = str_arg(it, args, 0)?;
        let found = selector_nodes(it, n, &s, false);
        Ok(list(it, found))
    });
    it.method(p, "getElementById", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let id = str_arg(it, args, 0)?;
        let d = doc(it);
        let found = d.dom.descendants(n).into_iter().find(|&e| d.dom.attr(e, "id") == Some(&*id));
        Ok(wrap_opt(it, found))
    });
    it.method(p, "getElementsByTagName", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let t = str_arg(it, args, 0)?.to_ascii_lowercase();
        let d = doc(it);
        let found: Vec<NodeId> = d.dom.descendants(n).into_iter().filter(|&e| e != n && (t == "*" || d.dom.tag(e) == t)).collect();
        Ok(list(it, found))
    });
    it.method(p, "getElementsByClassName", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let want: Vec<String> = str_arg(it, args, 0)?.split_ascii_whitespace().map(String::from).collect();
        let d = doc(it);
        let found: Vec<NodeId> = d.dom.descendants(n).into_iter().filter(|&e| e != n && d.dom.element(e).is_some_and(|el| want.iter().all(|c| el.has_class(c)))).collect();
        Ok(list(it, found))
    });
    it.method(p, "getElementsByName", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let name = str_arg(it, args, 0)?;
        let d = doc(it);
        let found: Vec<NodeId> = d.dom.descendants(n).into_iter().filter(|&e| d.dom.attr(e, "name") == Some(&*name)).collect();
        Ok(list(it, found))
    });
    it.method(p, "matches", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let s = str_arg(it, args, 0)?;
        let list = css::parse_selector_list(&s);
        let d = doc(it);
        Ok(Value::Bool(list.iter().any(|sel| css::matches(&d.dom, n, sel))))
    });
    it.method(p, "closest", 1, |it, this, args, _| {
        let n = this_node(it, this)?;
        let s = str_arg(it, args, 0)?;
        let list = css::parse_selector_list(&s);
        let d = doc(it);
        let mut cur = Some(n);
        while let Some(c) = cur {
            if d.dom.element(c).is_some() && list.iter().any(|sel| css::matches(&d.dom, c, sel)) {
                return Ok(wrap(it, c));
            }
            cur = d.dom.nodes[c].parent;
        }
        Ok(Value::Null)
    });
    it.method(p, "insertAdjacentHTML", 2, |it, this, args, _| {
        let n = this_node(it, this)?;
        let pos = str_arg(it, args, 0)?.to_ascii_lowercase();
        let html = str_arg(it, args, 1)?;
        let (parent, before) = adjacent(it, n, &pos)?;
        parse_html(it, parent, before, &html)?;
        Ok(Value::Undef)
    });
    it.method(p, "insertAdjacentElement", 2, |it, this, args, _| {
        let n = this_node(it, this)?;
        let pos = str_arg(it, args, 0)?.to_ascii_lowercase();
        let kids = nodes_from(it, &args[1.min(args.len())..])?;
        let (parent, before) = adjacent(it, n, &pos)?;
        insert_all(it, parent, kids, before)?;
        Ok(arg(args, 1))
    });
    it.method(p, "insertAdjacentText", 2, |it, this, args, _| {
        let n = this_node(it, this)?;
        let pos = str_arg(it, args, 0)?.to_ascii_lowercase();
        let t = str_arg(it, args, 1)?;
        let tn = doc(it).dom.create(Data::Text(t.to_string()));
        let (parent, before) = adjacent(it, n, &pos)?;
        insert_all(it, parent, vec![tn], before)?;
        Ok(Value::Undef)
    });
    it.method(p, "click", 0, |it, this, _, _| {
        let n = this_node(it, this)?;
        click(it, n, true)?;
        Ok(Value::Undef)
    });
    it.method(p, "focus", 0, |it, this, _, _| {
        let n = this_node(it, this)?;
        let d = doc(it);
        if matches!(d.dom.tag(n), "input" | "textarea") {
            d.focus = Some(n);
            d.caret = d.value(n).chars().count();
            host(it).dirty = true;
        }
        Ok(Value::Undef)
    });
    it.method(p, "blur", 0, |it, this, _, _| {
        let n = this_node(it, this)?;
        let d = doc(it);
        if d.focus == Some(n) {
            d.focus = None;
            host(it).dirty = true;
        }
        Ok(Value::Undef)
    });
    it.method(p, "submit", 0, |it, this, _, _| {
        let n = this_node(it, this)?;
        if let Some(h) = doc(it).submit(n) {
            host(it).hits.push(h);
        }
        Ok(Value::Undef)
    });
    it.method(p, "requestSubmit", 0, |it, this, _, _| {
        let n = this_node(it, this)?;
        if fire(it, n, "submit", true)? {
            if let Some(h) = doc(it).submit(n) {
                host(it).hits.push(h);
            }
        }
        Ok(Value::Undef)
    });
    it.method(p, "reset", 0, |_, _, _, _| Ok(Value::Undef));
    it.method(p, "getBoundingClientRect", 0, |it, this, _, _| {
        let n = this_node(it, this)?;
        let r = rect_of(doc(it), n).unwrap_or(justgfx::Rect::new(0, 0, 0, 0));
        let o = it.new_obj();
        for (k, v) in [("x", r.x), ("y", r.y), ("left", r.x), ("top", r.y), ("width", r.w), ("height", r.h), ("right", r.right()), ("bottom", r.bottom())] {
            o.borrow_mut().props.insert(Key::str(k), Prop::data(Value::Num(v as f64)));
        }
        Ok(Value::Obj(o))
    });
    for name in ["scrollIntoView", "scrollTo", "scrollBy", "scroll", "select", "setSelectionRange", "setCustomValidity", "showModal", "show", "close", "normalize", "setPointerCapture", "releasePointerCapture"] {
        it.method(p, name, 0, |_, _, _, _| Ok(Value::Undef));
    }
    for name in ["checkValidity", "reportValidity"] {
        it.method(p, name, 0, |_, _, _, _| Ok(Value::Bool(true)));
    }
    it.method(p, "getContext", 1, |_, _, _, _| Ok(Value::Null));
    it.method(p, "getClientRects", 0, |it, _, _, _| Ok(it.new_array(Vec::new())));
    // The document's own.
    it.method(p, "createElement", 1, |it, _, args, _| {
        let t = str_arg(it, args, 0)?.to_ascii_lowercase();
        let n = doc(it).dom.create(Data::Element(Element { tag: t, attrs: Vec::new() }));
        Ok(wrap(it, n))
    });
    it.method(p, "createElementNS", 2, |it, _, args, _| {
        let t = str_arg(it, args, 1)?.to_ascii_lowercase();
        let n = doc(it).dom.create(Data::Element(Element { tag: t, attrs: Vec::new() }));
        Ok(wrap(it, n))
    });
    it.method(p, "createTextNode", 1, |it, _, args, _| {
        let t = str_arg(it, args, 0)?;
        let n = doc(it).dom.create(Data::Text(t.to_string()));
        Ok(wrap(it, n))
    });
    it.method(p, "createComment", 1, |it, _, _, _| {
        let n = doc(it).dom.create(Data::Text(String::new()));
        Ok(wrap(it, n))
    });
    it.method(p, "createDocumentFragment", 0, |it, _, _, _| {
        let n = doc(it).dom.create(Data::Element(Element { tag: "#fragment".into(), attrs: Vec::new() }));
        Ok(wrap(it, n))
    });
    it.method(p, "createEvent", 1, |it, _, _, _| make_event(it, "", false));
    it.method(p, "hasFocus", 0, |_, _, _, _| Ok(Value::Bool(true)));
    for (name, f) in event_methods() {
        it.method(p, name, 2, f);
    }
}

type Method = fn(&mut Interp, &Value, &[Value], &Value) -> R<Value>;

fn event_methods() -> [(&'static str, Method); 3] {
    [
        ("addEventListener", |it, this, args, _| {
            let n = target_of(it, this)?;
            let kind = str_arg(it, args, 0)?.to_string();
            let f = arg(args, 1);
            if !matches!(f, Value::Obj(_)) {
                return Ok(Value::Undef);
            }
            let (capture, once) = match arg(args, 2) {
                o @ Value::Obj(_) => (it.get(&o, &Key::str("capture"))?.truthy(), it.get(&o, &Key::str("once"))?.truthy()),
                v => (v.truthy(), false),
            };
            let h = host(it);
            if h.listeners.iter().any(|l| l.node == n && l.kind == kind && l.capture == capture && l.f.strict_eq(&f)) {
                return Ok(Value::Undef);
            }
            h.listeners.push(Listener { node: n, kind: kind.clone(), f, capture, once });
            listen(it, n, &kind);
            Ok(Value::Undef)
        }),
        ("removeEventListener", |it, this, args, _| {
            let n = target_of(it, this)?;
            let kind = str_arg(it, args, 0)?.to_string();
            let f = arg(args, 1);
            let capture = match arg(args, 2) {
                o @ Value::Obj(_) => it.get(&o, &Key::str("capture"))?.truthy(),
                v => v.truthy(),
            };
            host(it).listeners.retain(|l| !(l.node == n && l.kind == kind && l.capture == capture && l.f.strict_eq(&f)));
            Ok(Value::Undef)
        }),
        ("dispatchEvent", |it, this, args, _| {
            let n = target_of(it, this)?;
            let ev = arg(args, 0);
            Ok(Value::Bool(dispatch(it, n, &ev)?))
        }),
    ]
}

fn node_arg(it: &mut Interp, args: &[Value], i: usize) -> R<NodeId> {
    node_of(it, &arg(args, i))
}

/// Where `insertAdjacent…` puts things: (parent, before).
fn adjacent(it: &mut Interp, n: NodeId, pos: &str) -> R<(NodeId, Option<NodeId>)> {
    let d = doc(it);
    let parent = d.dom.nodes[n].parent;
    let next = parent.and_then(|p| {
        let s = &d.dom.nodes[p].children;
        s.iter().position(|&c| c == n).and_then(|i| s.get(i + 1).copied())
    });
    Ok(match pos {
        "beforebegin" => (parent.ok_or_else(|| it.type_error("no parent"))?, Some(n)),
        "afterbegin" => (n, d.dom.nodes[n].children.first().copied()),
        "beforeend" => (n, None),
        "afterend" => (parent.ok_or_else(|| it.type_error("no parent"))?, next),
        _ => return Err(it.syntax_error("insertAdjacent: posizione sconosciuta")),
    })
}

fn install_helpers(it: &mut Interp, style: &ObjRef, list: &ObjRef) {
    it.method(style, "setProperty", 2, |it, this, args, _| {
        let n = helper_node(it, this)?;
        let k = str_arg(it, args, 0)?;
        let v = if arg(args, 1).nullish() { String::new() } else { str_arg(it, args, 1)?.to_string() };
        style_put(it, n, &k.to_ascii_lowercase(), &v);
        Ok(Value::Undef)
    });
    it.method(style, "getPropertyValue", 1, |it, this, args, _| {
        let n = helper_node(it, this)?;
        let k = str_arg(it, args, 0)?;
        Ok(style_get(it, n, &k))
    });
    it.method(style, "removeProperty", 1, |it, this, args, _| {
        let n = helper_node(it, this)?;
        let k = str_arg(it, args, 0)?;
        let old = style_get(it, n, &k);
        style_put(it, n, &k.to_ascii_lowercase(), "");
        Ok(old)
    });
    it.method(list, "contains", 1, |it, this, args, _| {
        let n = helper_node(it, this)?;
        let c = str_arg(it, args, 0)?;
        Ok(Value::Bool(classes(it, n).iter().any(|x| **x == *c)))
    });
    it.method(list, "add", 1, |it, this, args, _| {
        let n = helper_node(it, this)?;
        let mut l = classes(it, n);
        for i in 0..args.len() {
            let c = str_arg(it, args, i)?.to_string();
            if !l.contains(&c) {
                l.push(c);
            }
        }
        set_classes(it, n, &l);
        Ok(Value::Undef)
    });
    it.method(list, "remove", 1, |it, this, args, _| {
        let n = helper_node(it, this)?;
        let mut l = classes(it, n);
        for i in 0..args.len() {
            let c = str_arg(it, args, i)?;
            l.retain(|x| **x != *c);
        }
        set_classes(it, n, &l);
        Ok(Value::Undef)
    });
    it.method(list, "toggle", 2, |it, this, args, _| {
        let n = helper_node(it, this)?;
        let c = str_arg(it, args, 0)?.to_string();
        let mut l = classes(it, n);
        let on = if args.len() > 1 { arg(args, 1).truthy() } else { !l.contains(&c) };
        l.retain(|x| *x != c);
        if on {
            l.push(c);
        }
        set_classes(it, n, &l);
        Ok(Value::Bool(on))
    });
    it.method(list, "replace", 2, |it, this, args, _| {
        let n = helper_node(it, this)?;
        let a = str_arg(it, args, 0)?.to_string();
        let b = str_arg(it, args, 1)?.to_string();
        let mut l = classes(it, n);
        let Some(i) = l.iter().position(|x| *x == a) else { return Ok(Value::Bool(false)) };
        l[i] = b;
        set_classes(it, n, &l);
        Ok(Value::Bool(true))
    });
    it.method(list, "item", 1, |it, this, args, _| {
        let n = helper_node(it, this)?;
        let i = num_arg(it, args, 0)?;
        Ok(classes(it, n).get(i as usize).map_or(Value::Null, |c| sval(c)))
    });
    it.method(list, "forEach", 1, |it, this, args, _| {
        let n = helper_node(it, this)?;
        let f = arg(args, 0);
        for (i, c) in classes(it, n).into_iter().enumerate() {
            it.call(&f, Value::Undef, &[sval(&c), Value::Num(i as f64), this.clone()])?;
        }
        Ok(Value::Undef)
    });
    it.method(list, "toString", 0, |it, this, _, _| {
        let n = helper_node(it, this)?;
        Ok(sval(&classes(it, n).join(" ")))
    });
    let values = it.native(
        "values",
        0,
        |it, this, _, _| {
            let n = helper_node(it, this)?;
            let items: Vec<Value> = classes(it, n).iter().map(|c| sval(c)).collect();
            let a = it.new_array(items);
            let f = it.get(&a, &Key::Sym(SYM_ITERATOR))?;
            it.call(&f, a, &[])
        },
        Value::Undef,
        false,
    );
    list.borrow_mut().props.insert(Key::Sym(SYM_ITERATOR), Prop::hidden(Value::Obj(values.clone())));
    list.borrow_mut().props.insert(Key::str("values"), Prop::hidden(Value::Obj(values)));
}

fn helper_node(it: &mut Interp, this: &Value) -> R<NodeId> {
    if let Value::Obj(o) = this {
        if let Kind::Host(t, n) = o.borrow().kind {
            if t != 0 {
                return Ok(n);
            }
        }
    }
    Err(it.type_error("Illegal invocation"))
}

fn install_window(it: &mut Interp) {
    let g = it.global.clone();
    for name in ["window", "self", "top", "parent", "frames"] {
        it.set_global(name, Value::Obj(g.clone()));
    }
    let d = wrap(it, Dom::ROOT);
    it.set_global("document", d);
    for (name, f) in event_methods() {
        it.method(&g, name, 2, f);
    }
    it.accessor(
        &g,
        "location",
        |it, _, _, _| Ok(Value::Obj(host(it).location.clone())),
        Some(|it, _, args, _| {
            let u = str_arg(it, args, 0)?;
            navigate(it, &u);
            Ok(Value::Undef)
        }),
        Value::Undef,
    );
    let loc = host(it).location.clone();
    it.method(&loc, "assign", 1, |it, _, args, _| {
        let u = str_arg(it, args, 0)?;
        navigate(it, &u);
        Ok(Value::Undef)
    });
    it.method(&loc, "replace", 1, |it, _, args, _| {
        let u = str_arg(it, args, 0)?;
        navigate(it, &u);
        Ok(Value::Undef)
    });
    it.method(&loc, "reload", 0, |it, _, _, _| {
        let u = doc(it).url.clone();
        navigate(it, &u);
        Ok(Value::Undef)
    });
    it.method(&loc, "toString", 0, |it, _, _, _| Ok(sval(&doc(it).url)));
    it.method(&g, "alert", 1, |it, _, args, _| {
        let m = str_arg(it, args, 0)?;
        it.log(format!("[alert] {m}"));
        Ok(Value::Undef)
    });
    it.method(&g, "confirm", 1, |it, _, args, _| {
        let m = str_arg(it, args, 0)?;
        it.log(format!("[confirm] {m}"));
        Ok(Value::Bool(true))
    });
    it.method(&g, "prompt", 1, |it, _, args, _| {
        let m = str_arg(it, args, 0)?;
        it.log(format!("[prompt] {m}"));
        Ok(Value::Null)
    });
    for name in ["scrollTo", "scrollBy", "scroll", "focus", "blur", "print", "postMessage", "close", "stop"] {
        it.method(&g, name, 0, |_, _, _, _| Ok(Value::Undef));
    }
    it.method(&g, "getComputedStyle", 1, |it, _, args, _| {
        let n = node_arg(it, args, 0)?;
        Ok(helper(it, STYLE, n))
    });
    it.method(&g, "__media", 1, |it, _, args, _| {
        let q = str_arg(it, args, 0)?;
        let (w, h) = viewport(doc(it));
        Ok(Value::Bool(css::media_ok(&q, Viewport { width: w, height: h })))
    });
    it.method(&g, "__viewport", 0, |it, _, _, _| {
        let (w, h) = viewport(doc(it));
        Ok(it.new_array(vec![Value::Num(w as f64), Value::Num(h as f64)]))
    });
    it.method(&g, "__now", 0, |it, _, _, _| Ok(Value::Num(it.now)));
    it.method(&g, "__setUrl", 1, |it, _, args, _| {
        if !arg(args, 0).nullish() {
            let u = str_arg(it, args, 0)?;
            let d = doc(it);
            let to = url::join(&d.base, &u);
            // Only within the same site, like the real thing.
            if url::host(&to) == url::host(&d.url) {
                d.url = to;
            }
        }
        Ok(Value::Undef)
    });
    it.method(&g, "__urljoin", 2, |it, _, args, _| {
        let base = if arg(args, 1).nullish() { doc_url(it) } else { str_arg(it, args, 1)?.to_string() };
        let link = str_arg(it, args, 0)?;
        Ok(sval(&url::join(&base, &link)))
    });
    it.method(&g, "__fetch", 3, |it, _, args, _| {
        let u = str_arg(it, args, 0)?;
        let method = str_arg(it, args, 1)?;
        let body = if arg(args, 2).nullish() { None } else { Some(str_arg(it, args, 2)?) };
        let to = url::join(&doc(it).base, &u);
        let Some(f) = host(it).fetcher else { return Err(it.type_error("Failed to fetch")) };
        match f(&method, &to, body.as_deref()) {
            Ok((status, ct, text, at)) => Ok(it.new_array(vec![Value::Num(status as f64), sval(&ct), sval(&text), sval(&at)])),
            Err(e) => Err(it.type_error(&format!("Failed to fetch: {e}"))),
        }
    });
}

fn doc_url(it: &Interp) -> String {
    doc(it).url.clone()
}

fn viewport(d: &Document) -> (f32, f32) {
    let w = d.layout.as_ref().map_or(1024, |l| l.width) as f32;
    (w, crate::style::VH.get())
}

/// Browser objects that are simplest written in JavaScript.
const PRELUDE: &str = r#"
(() => {
const g = globalThis;
class Event {
  constructor(type, o = {}) { this.type = String(type); this.bubbles = !!o.bubbles; this.cancelable = o.cancelable !== false; this.composed = !!o.composed; this.defaultPrevented = false; this.isTrusted = false; this.timeStamp = __now(); this.target = null; this.currentTarget = null; this.eventPhase = 0 }
  preventDefault() { if (this.cancelable) this.defaultPrevented = true }
  stopPropagation() { this.__stop = true }
  stopImmediatePropagation() { this.__stop = true; this.__now = true }
  composedPath() { const p = []; for (let n = this.target; n; n = n.parentNode) p.push(n); p.push(window); return p }
  initEvent(type, bubbles, cancelable) { this.type = type; this.bubbles = !!bubbles; this.cancelable = !!cancelable }
}
class CustomEvent extends Event { constructor(t, o = {}) { super(t, o); this.detail = o.detail === undefined ? null : o.detail } }
g.Event = Event; g.CustomEvent = CustomEvent;
for (const n of ['UIEvent', 'MouseEvent', 'PointerEvent', 'KeyboardEvent', 'FocusEvent', 'InputEvent', 'SubmitEvent', 'TouchEvent', 'WheelEvent', 'ErrorEvent', 'MessageEvent', 'ProgressEvent', 'PopStateEvent', 'HashChangeEvent', 'AnimationEvent', 'TransitionEvent']) g[n] = class extends Event {};
class EventTarget {
  addEventListener(t, f) { (this.__l ??= []).push([t, f]) }
  removeEventListener(t, f) { this.__l = (this.__l || []).filter(x => x[0] !== t || x[1] !== f) }
  dispatchEvent(e) { for (const [t, f] of this.__l || []) if (t === e.type) (typeof f === 'function' ? f : f.handleEvent).call(this, e); return !e.defaultPrevented }
}
g.EventTarget = EventTarget;
const np = __nodeProto;
for (const n of ['Node', 'Element', 'HTMLElement', 'SVGElement', 'Text', 'Comment', 'CharacterData', 'Document', 'HTMLDocument', 'DocumentFragment', 'HTMLDivElement', 'HTMLSpanElement', 'HTMLAnchorElement', 'HTMLButtonElement', 'HTMLInputElement', 'HTMLTextAreaElement', 'HTMLSelectElement', 'HTMLOptionElement', 'HTMLFormElement', 'HTMLImageElement', 'HTMLCanvasElement', 'HTMLScriptElement', 'HTMLStyleElement', 'HTMLLinkElement', 'HTMLBodyElement', 'HTMLHeadElement', 'HTMLHtmlElement', 'HTMLParagraphElement', 'HTMLUListElement', 'HTMLLIElement', 'HTMLTableElement', 'HTMLTemplateElement', 'HTMLIFrameElement', 'HTMLVideoElement', 'HTMLAudioElement', 'HTMLMediaElement', 'HTMLLabelElement', 'HTMLDialogElement', 'HTMLDetailsElement']) {
  const f = function () { throw new TypeError('Illegal constructor') };
  f.prototype = np;
  g[n] = f;
}
Node.ELEMENT_NODE = 1; Node.TEXT_NODE = 3; Node.COMMENT_NODE = 8; Node.DOCUMENT_NODE = 9; Node.DOCUMENT_FRAGMENT_NODE = 11;
g.requestAnimationFrame = f => setTimeout(() => f(__now()), 16);
g.cancelAnimationFrame = id => clearTimeout(id);
g.requestIdleCallback = f => setTimeout(() => f({ didTimeout: false, timeRemaining: () => 10 }), 1);
g.cancelIdleCallback = id => clearTimeout(id);
g.performance = { now: () => __now(), timeOrigin: Date.now() - __now(), mark() {}, measure() {}, getEntriesByName: () => [], getEntriesByType: () => [] };
g.navigator = { userAgent: 'Mozilla/5.0 (justOS; x86_64) justweb/1.0', appName: 'Netscape', appVersion: '5.0 (justOS)', vendor: 'justOS', language: 'it-IT', languages: ['it-IT', 'it', 'en'], platform: 'justOS', onLine: true, cookieEnabled: false, doNotTrack: '1', hardwareConcurrency: 1, maxTouchPoints: 0, webdriver: false, clipboard: { writeText: () => Promise.resolve(), readText: () => Promise.resolve('') }, sendBeacon: () => true, serviceWorker: undefined };
g.screen = { width: 1024, height: 768, availWidth: 1024, availHeight: 768, colorDepth: 32, pixelDepth: 32, orientation: { type: 'landscape-primary', angle: 0 } };
g.devicePixelRatio = 1; g.scrollX = 0; g.scrollY = 0; g.pageXOffset = 0; g.pageYOffset = 0; g.name = ''; g.closed = false; g.isSecureContext = true; g.origin = location.origin;
Object.defineProperty(g, 'innerWidth', { get: () => __viewport()[0] });
Object.defineProperty(g, 'innerHeight', { get: () => __viewport()[1] });
Object.defineProperty(g, 'outerWidth', { get: () => __viewport()[0] });
Object.defineProperty(g, 'outerHeight', { get: () => __viewport()[1] });
g.matchMedia = q => ({ matches: __media(String(q)), media: String(q), onchange: null, addListener() {}, removeListener() {}, addEventListener() {}, removeEventListener() {} });
const storage = () => { const m = new Map(); return { getItem: k => m.has(String(k)) ? m.get(String(k)) : null, setItem: (k, v) => { m.set(String(k), String(v)) }, removeItem: k => { m.delete(String(k)) }, clear: () => m.clear(), key: i => [...m.keys()][i] ?? null, get length() { return m.size } } };
g.localStorage = storage(); g.sessionStorage = storage();
g.history = { length: 1, state: null, scrollRestoration: 'auto', pushState(s, t, u) { this.state = s; __setUrl(u) }, replaceState(s, t, u) { this.state = s; __setUrl(u) }, back() {}, forward() {}, go() {} };
g.customElements = { define() {}, get() {}, whenDefined: () => Promise.resolve(), upgrade() {} };
g.CSS = { supports: () => false, escape: s => String(s).replace(/[^\w-]/g, c => '\\' + c) };
g.getSelection = () => ({ rangeCount: 0, removeAllRanges() {}, addRange() {}, toString: () => '' });
g.crypto = { getRandomValues(a) { for (let i = 0; i < a.length; i++) a[i] = Math.floor(Math.random() * 256); return a }, randomUUID() { const h = '0123456789abcdef'; let s = ''; for (let i = 0; i < 36; i++) s += [8, 13, 18, 23].includes(i) ? '-' : i === 14 ? '4' : h[Math.floor(Math.random() * 16)]; return s } };
class Observer { constructor(f) { this.f = f } observe() {} unobserve() {} disconnect() {} takeRecords() { return [] } }
g.MutationObserver = Observer; g.ResizeObserver = Observer; g.PerformanceObserver = Observer;
g.IntersectionObserver = class extends Observer { observe(el) { setTimeout(() => this.f([{ target: el, isIntersecting: true, intersectionRatio: 1, boundingClientRect: el.getBoundingClientRect(), time: __now() }], this), 0) } };
const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
g.btoa = s => { s = String(s); let o = ''; for (let i = 0; i < s.length; i += 3) { const a = s.charCodeAt(i), b = s.charCodeAt(i + 1), c = s.charCodeAt(i + 2); if (a > 255 || b > 255 || c > 255) throw new Error('InvalidCharacterError'); const n = (a << 16) | ((b || 0) << 8) | (c || 0); o += B64[n >> 18 & 63] + B64[n >> 12 & 63] + (i + 1 < s.length ? B64[n >> 6 & 63] : '=') + (i + 2 < s.length ? B64[n & 63] : '=') } return o };
g.atob = s => { s = String(s).replace(/[\s=]/g, ''); let o = '', bits = 0, n = 0; for (const ch of s) { const v = B64.indexOf(ch); if (v < 0) throw new Error('InvalidCharacterError'); n = (n << 6) | v; bits += 6; if (bits >= 8) { bits -= 8; o += String.fromCharCode((n >> bits) & 255) } } return o };
class URLSearchParams {
  constructor(init = '') { this.p = []; if (typeof init === 'string') { for (const part of init.replace(/^\?/, '').split('&')) if (part) { const i = part.indexOf('='); const d = x => decodeURIComponent(x.replace(/\+/g, ' ')); this.p.push(i < 0 ? [d(part), ''] : [d(part.slice(0, i)), d(part.slice(i + 1))]) } } else if (Array.isArray(init)) this.p = init.map(([k, v]) => [String(k), String(v)]); else if (init) this.p = Object.entries(init).map(([k, v]) => [k, String(v)]) }
  get(k) { const e = this.p.find(x => x[0] === k); return e ? e[1] : null }
  getAll(k) { return this.p.filter(x => x[0] === k).map(x => x[1]) }
  has(k) { return this.p.some(x => x[0] === k) }
  set(k, v) { const i = this.p.findIndex(x => x[0] === k); if (i < 0) this.p.push([k, String(v)]); else { this.p[i][1] = String(v); this.p = this.p.filter((x, j) => j <= i || x[0] !== k) } }
  append(k, v) { this.p.push([k, String(v)]) }
  delete(k) { this.p = this.p.filter(x => x[0] !== k) }
  forEach(f) { for (const [k, v] of this.p) f(v, k, this) }
  keys() { return this.p.map(x => x[0])[Symbol.iterator]() }
  values() { return this.p.map(x => x[1])[Symbol.iterator]() }
  entries() { return this.p.map(x => [x[0], x[1]])[Symbol.iterator]() }
  [Symbol.iterator]() { return this.entries() }
  get size() { return this.p.length }
  toString() { const e = x => encodeURIComponent(x).replace(/%20/g, '+'); return this.p.map(([k, v]) => e(k) + '=' + e(v)).join('&') }
}
g.URLSearchParams = URLSearchParams;
class URL {
  constructor(u, base) { const href = __urljoin(String(u), base === undefined ? null : String(base)); const m = /^([a-z][a-z0-9+.-]*:)(?:\/\/([^/?#]*))?([^?#]*)(\?[^#]*)?(#.*)?$/i.exec(href); if (!m) throw new TypeError('Invalid URL: ' + u); this.protocol = m[1].toLowerCase(); this.host = m[2] || ''; this.pathname = m[3] || '/'; this.hash = m[5] && m[5].length > 1 ? m[5] : ''; this.searchParams = new URLSearchParams(m[4] || ''); const hp = this.host.split(':'); this.hostname = hp[0]; this.port = hp[1] || ''; this.username = ''; this.password = '' }
  get search() { const s = this.searchParams.toString(); return s ? '?' + s : '' }
  set search(v) { this.searchParams = new URLSearchParams(String(v)) }
  get origin() { return this.protocol + '//' + this.host }
  get href() { return this.protocol + (this.host ? '//' + this.host : '') + this.pathname + this.search + this.hash }
  toString() { return this.href }
  toJSON() { return this.href }
  static canParse(u, b) { try { new URL(u, b); return true } catch (e) { return false } }
}
g.URL = URL;
class Headers { constructor(h = {}) { this.h = {}; for (const [k, v] of Object.entries(h)) this.h[k.toLowerCase()] = String(v) } get(k) { return this.h[k.toLowerCase()] ?? null } has(k) { return k.toLowerCase() in this.h } set(k, v) { this.h[k.toLowerCase()] = String(v) } append(k, v) { this.set(k, v) } forEach(f) { for (const k in this.h) f(this.h[k], k, this) } }
g.Headers = Headers;
const response = r => { const body = r[2]; return { ok: r[0] >= 200 && r[0] < 300, status: r[0], statusText: '', url: r[3], redirected: false, type: 'basic', headers: new Headers({ 'content-type': r[1] }), text: () => Promise.resolve(body), json: () => Promise.resolve().then(() => JSON.parse(body)), clone() { return response(r) } } };
g.fetch = (input, init = {}) => new Promise((ok, fail) => { setTimeout(() => { try { const u = typeof input === 'string' ? input : String(input.url || input); const b = init.body == null ? null : typeof init.body === 'string' ? init.body : init.body instanceof URLSearchParams ? init.body.toString() : JSON.stringify(init.body); ok(response(__fetch(u, String(init.method || 'GET').toUpperCase(), b))) } catch (e) { fail(e) } }, 0) });
g.XMLHttpRequest = class {
  constructor() { this.readyState = 0; this.status = 0; this.statusText = ''; this.responseText = ''; this.response = ''; this.responseType = ''; this.responseURL = ''; this.timeout = 0; this.withCredentials = false; this.upload = {} }
  open(m, u) { this._m = String(m).toUpperCase(); this._u = String(u); this.readyState = 1 }
  setRequestHeader() {}
  overrideMimeType() {}
  getResponseHeader(n) { return String(n).toLowerCase() === 'content-type' ? this._ct || null : null }
  getAllResponseHeaders() { return this._ct ? 'content-type: ' + this._ct + '\r\n' : '' }
  addEventListener(t, f) { this['on' + t] = f }
  removeEventListener() {}
  abort() {}
  send(body = null) {
    setTimeout(() => {
      try { const r = __fetch(this._u, this._m, body == null ? null : String(body)); this.status = r[0]; this._ct = r[1]; this.responseText = r[2]; this.responseURL = r[3]; this.response = this.responseType === 'json' ? JSON.parse(r[2]) : r[2]; this.readyState = 4; this.onreadystatechange && this.onreadystatechange({ target: this }); this.onload && this.onload({ target: this }) }
      catch (e) { this.readyState = 4; this.onreadystatechange && this.onreadystatechange({ target: this }); this.onerror && this.onerror({ target: this, error: e }) }
      this.onloadend && this.onloadend({ target: this });
    }, 0);
  }
};
g.Image = function (w, h) { const i = document.createElement('img'); if (w) i.width = w; if (h) i.height = h; return i };
})();
"#;

// ---------------------------------------------------------------------------
// The page's interpreter

/// The scripts of one page. Not `Send`: it stays on the thread that made
/// it.
pub struct Scripts {
    it: Interp,
}

impl Scripts {
    /// Sets scripts up for a page: `now` is the steady clock in
    /// milliseconds, `epoch` the wall-clock time (ms since 1970) when `now`
    /// was 0.
    pub fn new(doc: &mut Document, now: f64, epoch: f64, fetcher: Option<Fetcher>) -> Scripts {
        let mut it = Interp::new();
        it.now = now;
        it.epoch = epoch;
        it.seed ^= (epoch as u64).rotate_left(17) ^ (now as u64);
        let object = it.p.object.clone();
        let mk = |it: &mut Interp| it.alloc(Some(object.clone()), Kind::Plain);
        let node_proto = mk(&mut it);
        let style_proto = mk(&mut it);
        let list_proto = mk(&mut it);
        let location = it.alloc(Some(object.clone()), Kind::Host(LOCATION, 0));
        it.host = Some(Box::new(Host {
            doc: core::ptr::null_mut(),
            nodes: BTreeMap::new(),
            listeners: Vec::new(),
            handlers: BTreeMap::new(),
            node_proto: node_proto.clone(),
            style_proto: style_proto.clone(),
            list_proto: list_proto.clone(),
            location,
            event: Value::Undef,
            fetcher,
            hits: Vec::new(),
            dirty: false,
            sheets_dirty: false,
            ready: "loading",
        }));
        it.host_get = Some(host_get);
        it.host_set = Some(host_set);
        it.global_miss = Some(named_element);
        install_node_methods(&mut it, &node_proto);
        install_helpers(&mut it, &style_proto, &list_proto);
        install_window(&mut it);
        it.set_global("__nodeProto", Value::Obj(node_proto));
        // <noscript> is for browsers without scripts.
        let gone: Vec<NodeId> = doc.dom.descendants(Dom::ROOT).into_iter().filter(|&n| doc.dom.tag(n) == "noscript").collect();
        for n in gone {
            doc.dom.detach(n);
        }
        doc.scripted = true;
        doc.invalidate();
        let mut s = Scripts { it };
        s.lend(doc, |it| {
            let _ = it.run(PRELUDE);
            let g = Value::Obj(it.global.clone());
            host(it).event = it.get(&g, &Key::str("Event")).unwrap_or(Value::Undef);
        });
        s
    }

    /// Lends the page to the interpreter for `f`, then marks what changed.
    fn lend<T>(&mut self, doc: &mut Document, f: impl FnOnce(&mut Interp) -> T) -> T {
        host(&mut self.it).doc = doc as *mut Document;
        let r = f(&mut self.it);
        let h = host(&mut self.it);
        h.doc = core::ptr::null_mut();
        let (dirty, sheets) = (core::mem::take(&mut h.dirty), core::mem::take(&mut h.sheets_dirty));
        if sheets {
            doc.reread_styles();
        }
        if dirty {
            doc.invalidate();
        }
        r
    }

    /// Runs a script of the page.
    pub fn run(&mut self, doc: &mut Document, src: &str, now: f64) {
        self.it.now = now;
        self.lend(doc, |it| {
            let _ = it.run(src);
        });
    }

    /// The scripts have all run: `DOMContentLoaded`, then `load`.
    pub fn loaded(&mut self, doc: &mut Document, now: f64) {
        self.it.now = now;
        self.lend(doc, |it| {
            host(it).ready = "interactive";
            let r = (|| -> R<()> {
                fire(it, Dom::ROOT, "readystatechange", false)?;
                fire(it, Dom::ROOT, "DOMContentLoaded", true)?;
                host(it).ready = "complete";
                fire(it, Dom::ROOT, "readystatechange", false)?;
                fire(it, WINDOW, "load", false)?;
                fire(it, WINDOW, "pageshow", false)?;
                Ok(())
            })();
            if let Err(c) = r {
                let _ = report(it, c);
            }
            it.drain();
        });
    }

    /// Hands the page's events to its scripts and runs the timers that are
    /// due. Returns whether any script ran.
    pub fn pump(&mut self, page: &mut Document, now: f64) -> bool {
        let events = core::mem::take(&mut page.events);
        let due = self.it.next_timer().is_some_and(|t| t <= now);
        if events.is_empty() && !due {
            return false;
        }
        self.it.now = now;
        self.lend(page, |it| {
            for ev in events {
                it.steps = 0;
                let r = match ev.kind {
                    "click" => click(it, ev.node, ev.deferred),
                    "submit" => (|| {
                        let d = doc(it);
                        let Some(form) = d.form_of(ev.node) else { return Ok(()) };
                        if fire(it, form, "submit", true)? {
                            if let Some(h) = doc(it).submit(ev.node) {
                                host(it).hits.push(h);
                            }
                        }
                        Ok(())
                    })(),
                    kind => fire(it, ev.node, kind, true).map(|_| ()),
                };
                if let Err(c) = r {
                    let _ = report(it, c);
                }
                it.drain();
            }
            it.run_timers(now);
        });
        true
    }

    /// When the next timer is due (steady milliseconds).
    pub fn next_due(&self) -> Option<f64> {
        self.it.next_timer()
    }

    /// What the scripts asked the browser to do.
    pub fn take_hits(&mut self) -> Vec<Hit> {
        core::mem::take(&mut host(&mut self.it).hits)
    }

    /// What the scripts printed since last asked.
    pub fn take_console(&mut self) -> Vec<String> {
        core::mem::take(&mut self.it.console)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Script;

    fn page(html: &str) -> (Document, Scripts) {
        let mut d = Document::new("https://example.org/a/b.html?q=1#top", html.as_bytes(), "text/html");
        let mut s = Scripts::new(&mut d, 0.0, 1_700_000_000_000.0, None);
        for sc in d.scripts() {
            if let Script::Inline(src) = sc {
                s.run(&mut d, &src, 0.0);
            }
        }
        s.loaded(&mut d, 0.0);
        (d, s)
    }

    fn console(s: &mut Scripts) -> String {
        s.take_console().join("\n")
    }

    #[test]
    fn reads_and_changes_the_tree() {
        let (d, mut s) = page(
            "<title>T</title><body><ul id=l><li class='a b'>uno</li><li>due</li></ul><p id=p>x</p>
             <script>
               const l = document.getElementById('l');
               console.log(l.children.length, l.firstElementChild.textContent, l.querySelector('.b').className, document.title);
               const li = document.createElement('li'); li.textContent = 'tre'; li.classList.add('n'); l.appendChild(li);
               document.querySelector('#p').innerHTML = '<b>ciao</b> mondo';
               console.log(l.children.length, document.querySelectorAll('li').length, p.innerHTML, li.outerHTML);
               document.title = 'Nuovo';
               l.firstElementChild.remove();
               l.style.color = 'red'; l.style.backgroundColor = 'blue';
               console.log(l.getAttribute('style'), l.style.color, l.dataset.x === undefined);
               l.dataset.fooBar = '1'; console.log(l.getAttribute('data-foo-bar'), location.hostname, location.pathname, location.search);
             </script>",
        );
        assert_eq!(console(&mut s), "2 uno a b T\n3 3 <b>ciao</b> mondo <li class=\"n\">tre</li>\ncolor: red; background-color: blue red true\n1 example.org /a/b.html ?q=1");
        assert_eq!(d.title, "Nuovo");
        assert!(d.text().contains("due") && !d.text().contains("uno") && d.text().contains("tre"));
    }

    #[test]
    fn events_bubble_and_cancel() {
        let (mut d, mut s) = page(
            "<body><div id=box><a id=a href='/next'>vai</a><button id=b onclick=\"console.log('attr', event.type)\">b</button></div>
             <script>
               document.getElementById('box').addEventListener('click', e => console.log('box', e.target.id, e.currentTarget.id));
               document.getElementById('a').addEventListener('click', e => { console.log('a'); e.preventDefault() });
               document.addEventListener('DOMContentLoaded', () => console.log('ready', document.readyState));
               window.addEventListener('load', () => console.log('load', document.readyState));
             </script>",
        );
        assert_eq!(console(&mut s), "ready interactive\nload complete");
        let a = d.dom.descendants(Dom::ROOT).into_iter().find(|&n| d.dom.attr(n, "id") == Some("a")).unwrap();
        let b = d.dom.descendants(Dom::ROOT).into_iter().find(|&n| d.dom.attr(n, "id") == Some("b")).unwrap();
        assert!(d.listened.contains(&a));
        d.events.push(crate::document::Event { node: a, kind: "click", deferred: true });
        d.events.push(crate::document::Event { node: b, kind: "click", deferred: true });
        assert!(s.pump(&mut d, 5.0));
        assert_eq!(console(&mut s), "a\nbox a box\nattr click\nbox b box");
        // The link's click was cancelled; the button sends no form.
        assert!(s.take_hits().is_empty());
    }

    #[test]
    fn timers_and_navigation() {
        let (mut d, mut s) = page(
            "<body><p id=p>0</p><script>
               let n = 0; const id = setInterval(() => { p.textContent = ++n; if (n === 3) { clearInterval(id); location.href = 'fine.html' } }, 100);
               setTimeout(() => console.log('once'), 50);
             </script>",
        );
        assert_eq!(s.next_due(), Some(50.0));
        assert!(s.pump(&mut d, 60.0));
        assert_eq!(console(&mut s), "once");
        // An interval does not catch up on runs it missed.
        for t in [400.0, 520.0, 640.0] {
            s.pump(&mut d, t);
        }
        assert!(d.text().contains('3'));
        assert_eq!(s.take_hits(), vec![Hit::Open(String::from("https://example.org/a/fine.html"))]);
        assert_eq!(s.next_due(), None);
    }

    #[test]
    fn forms_and_fields() {
        let (mut d, mut s) = page(
            "<body><form id=f action='/cerca'><input id=q name=q value='ab'><select id=s><option value=x>X<option value=y selected>Y</select><button>Vai</button></form>
             <script>
               const f = document.forms[0];
               console.log(q.value, s.value, s.selectedIndex, f.elements.length);
               q.addEventListener('input', () => console.log('input', q.value));
               f.addEventListener('submit', e => { e.preventDefault(); console.log('submit', new URLSearchParams('q=' + q.value).get('q')) });
             </script>",
        );
        assert_eq!(console(&mut s), "ab y 1 3");
        let q = d.dom.descendants(Dom::ROOT).into_iter().find(|&n| d.dom.attr(n, "id") == Some("q")).unwrap();
        d.focus = Some(q);
        d.caret = 2;
        d.type_key("c");
        assert_eq!(d.type_key("\n"), Some(Hit::Script));
        s.pump(&mut d, 1.0);
        assert_eq!(console(&mut s), "input abc\nsubmit abc");
        assert!(s.take_hits().is_empty());
    }

    #[test]
    fn page_survives_bad_scripts() {
        let (_, mut s) = page("<body><script>throw new Error('rotto')</script><script>while(true){}</script><script>console.log('dopo', typeof fetch, typeof URL, btoa('ciao'), atob('Y2lhbw=='))</script>");
        assert_eq!(console(&mut s), "Uncaught Error: rotto\nscript interrotto: ci mette troppo\ndopo function function Y2lhbw== ciao");
    }
}
