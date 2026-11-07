mod common;

use just_vm::ui::{Attr, AttrValue, Color, ColorToken, UiKind};
use just_vm::{Limits, UiEvent, UiNode, UiValue, Value, Vm};
use justc_core::{compile_source, CompileOptions};
use std::cell::RefCell;
use std::rc::Rc;

fn vm_for(src: &str, opts: CompileOptions) -> (Vm, Rc<RefCell<Vec<String>>>) {
    let out = compile_source("test.jt", src, opts).unwrap_or_else(|e| panic!("{}", e.render()));
    let (host, lines) = common::StdHost::new();
    let vm = Vm::with_host(out.program, Box::new(host)).unwrap();
    (vm, lines)
}

/// Run in both debug and release mode and check the outputs agree.
fn run(src: &str) -> Vec<String> {
    let mut results = Vec::new();
    for opts in [CompileOptions::DEBUG, CompileOptions::RELEASE] {
        let (mut vm, lines) = vm_for(src, opts);
        vm.run_main().unwrap_or_else(|e| panic!("{e}"));
        results.push(lines.borrow().clone());
    }
    assert_eq!(results[0], results[1], "debug and release disagree");
    results.pop().unwrap()
}

fn run_err(src: &str) -> String {
    let (mut vm, _) = vm_for(src, CompileOptions::DEBUG);
    vm.run_main().expect_err("expected a runtime error").to_string()
}

#[test]
fn arithmetic_and_strings() {
    let out = run(r#"
let a = 7
let b = 2
print(a + b, a - b, a * b, a / b, a % b)
print(7.0 / 2, 1 + 0.5, -a)
print("x" + "y", "ab" * 3, "{a} squared is {a * a}")
print(1 == 1.0, "a" < "b", not (a > b))
"#);
    assert_eq!(out, ["9 5 14 3 1", "3.5 1.5 -7", "xy ababab 7 squared is 49", "true true false"]);
}

#[test]
fn recursion_and_loops() {
    let out = run(r#"
fn fib(n)
    if n < 2
        return n
    return fib(n - 1) + fib(n - 2)

let total = 0
for i in 0..10
    if i == 3
        continue
    if i == 8
        break
    total += i
print(fib(20), total)

let n = 0
while n < 5
    n += 1
print(n)
"#);
    assert_eq!(out, ["6765 25", "5"]);
}

#[test]
fn lists_and_builtins() {
    let out = run(r#"
let xs = [3, 1, 2]
xs.push(10)
xs[0] = xs[0] * 100
xs[1] += 5
print(xs, len(xs), xs[-1])
print(join(["a", "b"], "-"), contains(xs, 10), type_of(xs), int("42") + 1, str(1.5))
let words = []
for c in "héllo"
    push(words, c)
print(len(words), words[1], max(3, 9), min("b", "a"), abs(-4))
"#);
    assert_eq!(out, ["[300, 6, 2, 10] 4 10", "a-b true list 43 1.5", "5 é 9 a 4"]);
}

#[test]
fn logical_short_circuit() {
    let out = run(r#"
fn boom()
    print("evaluated")
    return true
print(false and boom(), true or boom())
"#);
    assert_eq!(out, ["false true"]);
}

#[test]
fn runtime_errors_have_traces() {
    let e = run_err("fn inner(x)\n    return x / 0\nfn outer()\n    return inner(1)\nouter()\n");
    assert!(e.contains("division by zero"), "{e}");
    assert!(e.contains("at inner (line 2)"), "{e}");
    assert!(e.contains("at outer (line 4)"), "{e}");
    assert!(e.contains("at <main> (line 5)"), "{e}");

    assert!(run_err("if 1\n    print(1)\n").contains("condition must be a bool"));
    assert!(run_err("let xs = [1]\nprint(xs[5])\n").contains("out of bounds"));
    assert!(run_err("print(9223372036854775807 + 1)\n").contains("overflow"));
    assert!(run_err("fn f()\n    label \"x\"\nf()\n").contains("only be used while rendering"));
}

#[test]
fn stack_overflow_is_an_error_not_a_crash() {
    let e = run_err("fn f(n)\n    return f(n + 1)\nf(0)\n");
    assert!(e.contains("stack overflow"), "{e}");
}

#[test]
fn step_budget_stops_infinite_loops() {
    let (mut vm, _) = vm_for("while true\n    let x = 1\n", CompileOptions::DEBUG);
    vm.set_limits(Limits { max_steps: Some(100_000), ..Limits::default() });
    assert!(vm.run_main().unwrap_err().message.contains("budget"));
}

#[test]
fn reactive_view() {
    let src = r#"
let count = 0
let log = []

fn add(n)
    count += n
    push(log, n)

view
    window "Counter {count}"
        column
            label "Count: {count}"
            row
                button "+1" -> add(1)
                button "+10" -> add(10)
            if count > 10
                label "big!"
"#;
    let (mut vm, _) = vm_for(src, CompileOptions::DEBUG);
    vm.run_main().unwrap();
    let nodes = vm.render_view().unwrap();
    assert_eq!(UiNode::window_title(&nodes), Some("Counter 0"));
    let text = UiNode::render_text(&nodes);
    assert!(text.contains("Count: 0") && text.contains("[ +10 ]") && !text.contains("big"), "{text}");

    let row = &nodes[0].children[0].children[1];
    let plus_ten = row.children[1].action.unwrap();
    vm.dispatch(UiEvent::Click(plus_ten)).unwrap();
    let plus_one = vm.render_view().unwrap()[0].children[0].children[1].children[0].action.unwrap();
    vm.dispatch(UiEvent::Click(plus_one)).unwrap();
    let nodes = vm.render_view().unwrap();
    let text = UiNode::render_text(&nodes);
    assert!(text.contains("Count: 11") && text.contains("big!"), "{text}");
    assert!(matches!(vm.global("count"), Some(Value::Int(11))));
}

fn find(nodes: &[UiNode], kind: UiKind) -> &UiNode {
    nodes.iter().flat_map(UiNode::iter).find(|n| n.kind == kind).unwrap_or_else(|| panic!("no {kind:?} in {nodes:#?}"))
}

#[test]
fn two_way_bindings_and_change_handlers() {
    let src = r#"
let name = "Ada"
let flags = [false, false]
let volume = 3
let log = []

fn set_volume(prefix, v)
    volume = v
    push(log, "{prefix}{v}")

view
    window
        input name
        checkbox flags[1] text: "Second"
        slider volume min: 0, max: 10 -> set_volume("vol=")
        label "{name} {flags} {volume}"
"#;
    let (mut vm, _) = vm_for(src, CompileOptions::RELEASE);
    vm.run_main().unwrap();
    let nodes = vm.render_view().unwrap();
    let input = find(&nodes, UiKind::Input);
    assert_eq!(input.value, Some(UiValue::Text("Ada".into())));
    vm.dispatch(UiEvent::Change(input.action.unwrap(), UiValue::Text("Grace".into()))).unwrap();

    let nodes = vm.render_view().unwrap();
    let checkbox = find(&nodes, UiKind::Checkbox);
    assert_eq!((checkbox.value.clone(), checkbox.text_attr(Attr::Text)), (Some(UiValue::Bool(false)), Some("Second")));
    vm.dispatch(UiEvent::Change(checkbox.action.unwrap(), UiValue::Bool(true))).unwrap();

    let nodes = vm.render_view().unwrap();
    let slider = find(&nodes, UiKind::Slider);
    assert_eq!(slider.number(Attr::Max), Some(10.0));
    vm.dispatch(UiEvent::Change(slider.action.unwrap(), UiValue::Int(7))).unwrap();

    let nodes = vm.render_view().unwrap();
    assert_eq!(find(&nodes, UiKind::Label).text, "Grace [false, true] 7");
    assert_eq!(vm.global("log").unwrap().to_string(), r#"["vol=7"]"#);
}

#[test]
fn attributes_are_typed_and_dynamic() {
    let src = r##"
let compact = true
let accent = "primary"

fn badge(t)
    label t size: 12
        weight: "bold"

view
    window "App" theme: "light", accent: "#336699"
        card style: "outlined"
            padding: 4
            if not compact
                padding: 24
            label "Hi" color: accent, tooltip: 42
            badge("new")
"##;
    let (mut vm, _) = vm_for(src, CompileOptions::DEBUG);
    vm.run_main().unwrap();
    let nodes = vm.render_view().unwrap();
    assert_eq!(nodes[0].choice(Attr::Theme), Some("light"));
    assert_eq!(nodes[0].color(Attr::Accent), Some(Color::Rgba([0x33, 0x66, 0x99, 255])));
    let card = find(&nodes, UiKind::Card);
    assert_eq!((card.choice(Attr::Style), card.number(Attr::Padding)), (Some("outlined"), Some(4.0)));
    let label = &card.children[0];
    assert_eq!(label.attr(Attr::Color), Some(&AttrValue::Color(Color::token(ColorToken::Primary))));
    assert_eq!(label.text_attr(Attr::Tooltip), Some("42"));
    assert_eq!(card.children[1].choice(Attr::Weight), Some("bold"));
    assert_eq!(card.children[1].number(Attr::Size), Some(12.0));
}

#[test]
fn ui_runtime_errors() {
    let render_err = |src: &str| {
        let (mut vm, _) = vm_for(src, CompileOptions::DEBUG);
        vm.run_main().unwrap();
        vm.render_view().expect_err("expected a render error").to_string()
    };
    let e = render_err("let p = \"big\"\nview\n    card padding: p\n");
    assert!(e.contains("`padding` expects a number, found string"), "{e}");
    let e = render_err("let on = \"yes\"\nview\n    checkbox on\n");
    assert!(e.contains("`checkbox` expects a bool, found string"), "{e}");
    let e = render_err("let c = \"primay\"\nview\n    label \"x\" color: c\n");
    assert!(e.contains("did you mean \"primary\""), "{e}");
    let e = render_err("fn inner()\n    label \"y\"\nview\n    button \"x\" -> inner()\n        inner()\n");
    assert!(e.contains("`button` cannot contain child elements"), "{e}");
    let e = render_err("fn deco()\n    placeholder: \"x\"\nview\n    label \"x\"\n        deco()\n");
    assert!(e.contains("`label` has no attribute `placeholder`"), "{e}");
}

#[test]
fn format_one_ui_opcodes_still_run() {
    use justc_core::bytecode::{Constant, Function, Op, Program};
    // view: UiOpen window "Old", UiLabel "hi", UiButton "go" -> f, UiClose
    let view = vec![
        Op::Const as u8,
        0,
        0,
        Op::UiOpen as u8,
        UiKind::Window as u8,
        Op::Const as u8,
        1,
        0,
        Op::UiLabel as u8,
        Op::Const as u8,
        2,
        0,
        Op::Func as u8,
        2,
        0,
        Op::UiButton as u8,
        Op::UiClose as u8,
        Op::Nil as u8,
        Op::Return as u8,
    ];
    let ret = vec![Op::Nil as u8, Op::Return as u8];
    let program = Program {
        constants: vec![Constant::Str("Old".into()), Constant::Str("hi".into()), Constant::Str("go".into())],
        functions: vec![
            Function { name: "<main>".into(), code: ret.clone(), ..Default::default() },
            Function { name: "view".into(), code: view, ..Default::default() },
            Function { name: "f".into(), code: ret, ..Default::default() },
        ],
        types: Vec::new(),
        globals: Vec::new(),
        entry: 0,
        view: Some(1),
    };
    let mut vm = Vm::new(program).unwrap();
    vm.run_main().unwrap();
    let nodes = vm.render_view().unwrap();
    assert_eq!(UiNode::render_text(&nodes), "== Old ==\n  hi\n  [ go ]\n");
    let action = nodes[0].children[1].action.unwrap();
    vm.dispatch(UiEvent::Click(action)).unwrap();
}

#[test]
fn maps() {
    let out = run(r#"
let m = {"b": 2, "a": 1}
m["c"] = 3
m["a"] += 10
print(m, len(m), m["a"], contains(m, "b"), contains(m, "z"))
let ks = []
for k in m
    ks.push(k)
print(ks, keys(m), values(m), m.get("zz"), m.get("b"))
print(remove(m, "b"), remove(m, "b"), m == {"a": 11, "c": 3}, {1: "x", true: "y"})
print("{ {"k": 1}["k"] }")
"#);
    assert_eq!(
        out,
        [
            r#"{"a": 11, "b": 2, "c": 3} 3 11 true false"#,
            r#"["a", "b", "c"] ["a", "b", "c"] [11, 2, 3] None Some(2)"#,
            r#"Some(2) None true {true: "y", 1: "x"}"#,
            "1",
        ]
    );
    assert!(run_err("let m = {}\nprint(m[\"x\"])\n").contains("key \"x\" not found"));
    assert!(run_err("let m = {[1]: 2}\n").contains("map keys must be"));
}

#[test]
fn structs_and_methods() {
    let out = run(r#"
struct Point
    x
    y = 0

    fn sum(self)
        return self.x + self.y

    fn scaled(self, k)
        return Point(self.x * k, self.y * k)

    fn origin()
        return Point(x: 0)

let p = Point(y: 2, x: 1)
let q = p
q.x = 5
p.y += 1
print(p, q.sum(), p.scaled(2), Point.origin(), Point(9), type_of(p))
let r = copy(p)
r.x = 0
print(p.x, r.x, p == Point(5, 3), p == r)
fn shout(s)
    return s + "!"
print("hi".shout(), [1, 2].len())
"#);
    assert_eq!(out, ["Point(x: 5, y: 3) 8 Point(x: 10, y: 6) Point(x: 0, y: 0) Point(x: 9, y: 0) Point", "5 0 true false", "hi! 2",]);
    assert!(run_err("struct P\n    x\nstruct Q\n    y\nfn f(v)\n    return v.y\nf(P(1))\n").contains("`P` has no field `y`"));
    assert!(run_err("fn f(v)\n    return v.size()\nstruct P\n    x\n    fn size(self)\n        return 1\nf(3)\n")
        .contains("int has no method `size`"));
}

#[test]
fn enums_and_match() {
    let out = run(r#"
enum Shape
    Circle(r)
    Rect(w, h)
    Empty

    fn area(self)
        match self
            Circle(r) => return 3 * r * r
            Rect(w, h) => return w * h
            Empty => return 0

fn kind(s)
    match s
        Shape.Rect(w, h) if w == h => return "square {w}"
        Rect(w, _) => return "rect {w}"
        Circle(_) | Empty => return "other"

fn size(n)
    match n
        0 => return "zero"
        1 | 2 => return "small"
        3..10 => return "medium"
        "x" | nil => return "special"
        other => return "big {other}"

let shapes = [Shape.Circle(2), Shape.Rect(2, 3), Shape.Rect(4, 4), Shape.Empty]
for s in shapes
    print(s, kind(s), s.area())
print(size(0), size(2), size(9), size("x"), size(nil), size(10))
print(Shape.Empty == Shape.Empty, Shape.Circle(1) == Shape.Circle(1), Shape.Circle(1) == Shape.Circle(2))
let nested = Some(Shape.Rect(1, 2))
match nested
    Some(Rect(a, b)) => print("nested", a, b)
    _ => print("no")
if let Some(Circle(r)) = nested
    print("circle")
elif let Some(Rect(w, _)) = nested
    print("rect", w)
"#);
    assert_eq!(
        out,
        [
            "Circle(2) other 12",
            "Rect(2, 3) rect 2 6",
            "Rect(4, 4) square 4 16",
            "Empty other 0",
            "zero small medium special special big 10",
            "true true false",
            "nested 1 2",
            "rect 1",
        ]
    );
    assert!(run_err("fn f(n)\n    match n\n        1 => return 1\nf(2)\n").contains("no `match` arm matches the value 2"));
    // Covering every variant does not make other types match.
    let e = run_err("fn f(o)\n    match o\n        Some(x) => return x\n        None => return 0\nf(5)\n");
    assert!(e.contains("no `match` arm matches the value 5"), "{e}");
}

#[test]
fn option_result_and_try() {
    let out = run(r#"
fn parse(text)
    let n = int(text)
    if n < 0
        return Err("negative: {n}")
    return Ok(n)

fn add(a, b)
    return Ok(parse(a)? + parse(b)?)

fn first_even(items)
    for x in items
        if x % 2 == 0
            return Some(x)
    return None

fn double_first(items)
    return Some(first_even(items)? * 2)

print(add("2", "3"), add("2", "-3"), double_first([1, 4]), double_first([1, 3]))
print(unwrap(Some(1)), unwrap_or(None, 5), expect(Ok(2), "x"), is_some(Some(1)), is_none(Some(1)), is_ok(Err(1)), is_err(Err(1)))
print(Some(3).unwrap_or(0), get([1, 2], 5), get([1, 2], -1))
"#);
    assert_eq!(out, ["Ok(5) Err(\"negative: -3\") Some(8) None", "1 5 2 true false false true", "3 None Some(2)"]);
    assert!(run_err("print(unwrap(Err(\"bad\")))\n").contains("called `unwrap` on Err(\"bad\")"));
    assert!(run_err("print(expect(None, \"need a value\"))\n").contains("need a value: None"));
    assert!(run_err("fn f()\n    return 3?\nf()\n").contains("expects an Option or Result"));
    assert!(run_err("assert(1 > 2, \"math\")\n").contains("assertion failed: math"));
    assert!(run_err("panic(\"stop\")\n").contains("stop"));
}

#[test]
fn closures() {
    let out = run(r#"
fn counter(start)
    let state = [start]
    fn next()
        state[0] += 1
        return state[0]
    return next

let c = counter(10)
print(c(), c(), c())
let k = 3
let times = |x| x * k
print(times(2), (|a, b| a + b)(1, 2), (|| "none")())
fn map(items, f)
    let out = []
    for x in items
        out.push(f(x))
    return out
print(map([1, 2, 3], times), map([1, 2], |x| map([x, x], |y| y + x)))
fn fact(n)
    fn go(n, acc)
        if n <= 1
            return acc
        return go(n - 1, acc * n)
    return go(n, 1)
print(fact(10))
let fns = []
for i in 0..3
    fns.push(|| i * 10)
print(map(fns, |f| f()))
"#);
    assert_eq!(out, ["11 12 13", "6 3 none", "[3, 6, 9] [[2, 2], [4, 4]]", "3628800", "[0, 10, 20]"]);
}

#[test]
fn struct_fields_and_methods_in_views() {
    let src = r#"
struct Todo
    title
    done = false

    fn toggle(self)
        self.done = not self.done

    fn rename(self, prefix, value)
        self.title = prefix + value

let todo = Todo("Write docs")
let filter = {"text": ""}

view
    window "Todos"
        checkbox todo.done text: todo.title
        input todo.title -> todo.rename("> ")
        button "Toggle" -> todo.toggle()
        input filter["text"]
        button "Reset" -> (|| filter["text"] = "")
"#;
    let errors = compile_source("t.jt", src, CompileOptions::DEBUG).err().map(|e| e.render()).unwrap_or_default();
    assert!(errors.contains("the body of a lambda must be an expression"), "{errors}");
    let src = src.replace("        button \"Reset\" -> (|| filter[\"text\"] = \"\")\n", "");
    let (mut vm, _) = vm_for(&src, CompileOptions::DEBUG);
    vm.run_main().unwrap();
    let nodes = vm.render_view().unwrap();
    let checkbox = find(&nodes, UiKind::Checkbox);
    vm.dispatch(UiEvent::Change(checkbox.action.unwrap(), UiValue::Bool(true))).unwrap();
    assert_eq!(vm.global("todo").unwrap().to_string(), r#"Todo(title: "Write docs", done: true)"#);
    let nodes = vm.render_view().unwrap();
    let toggle = nodes[0].children.iter().find(|n| n.kind == UiKind::Button).unwrap();
    vm.dispatch(UiEvent::Click(toggle.action.unwrap())).unwrap();
    let nodes = vm.render_view().unwrap();
    let input = find(&nodes, UiKind::Input);
    vm.dispatch(UiEvent::Change(input.action.unwrap(), UiValue::Text("Ship".into()))).unwrap();
    assert_eq!(vm.global("todo").unwrap().to_string(), r#"Todo(title: "> Ship", done: false)"#);
    let nodes = vm.render_view().unwrap();
    let filter = nodes[0].children.iter().filter(|n| n.kind == UiKind::Input).nth(1).unwrap();
    vm.dispatch(UiEvent::Change(filter.action.unwrap(), UiValue::Text("abc".into()))).unwrap();
    assert_eq!(vm.global("filter").unwrap().to_string(), r#"{"text": "abc"}"#);
}

#[test]
fn modules() {
    let files = [
        ("shapes", "import util\nlet made = 0\nstruct Box\n    w\n    h = 1\nenum Kind\n    Wide\n    Tall\nfn make(w, h)\n    made += 1\n    return Box(w, h)\nfn kind(b)\n    if b.w > b.h\n        return Kind.Wide\n    return Kind.Tall\nprint(\"shapes ready\", util.twice(1))\n"),
        ("util", "fn twice(x)\n    return x * 2\nprint(\"util ready\")\n"),
    ];
    let main = "import shapes\nimport util.{twice}\nlet b = shapes.make(3, 2)\nprint(b, shapes.kind(b), twice(b.w), shapes.Box(w: 1))\nmatch shapes.kind(b)\n    shapes.Kind.Wide => print(\"wide\")\n    shapes.Kind.Tall => print(\"tall\")\nshapes.made += 1\nprint(shapes.made)\n";
    let mut load =
        |m: &str| files.iter().find(|(n, _)| *n == m).map(|(n, src)| (format!("{n}.jt"), src.to_string())).ok_or(format!("no {m}"));
    let out =
        justc_core::compile_with_modules("main.jt", main, CompileOptions::DEBUG, &mut load).unwrap_or_else(|e| panic!("{}", e.render()));
    let lines = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&lines);
    let mut vm = Vm::with_host(out.program, Box::new(move |s: &str| sink.borrow_mut().push(s.to_owned()))).unwrap();
    vm.run_main().unwrap();
    assert_eq!(*lines.borrow(), ["util ready", "shapes ready 2", "Box(w: 3, h: 2) Wide 6 Box(w: 1, h: 1)", "wide", "2"]);

    let bad =
        justc_core::compile_with_modules("main.jt", "import util\nprint(util.thrice(1))\n", CompileOptions::DEBUG, &mut load).unwrap_err();
    assert!(bad.render().contains("module `util` has no `thrice`"), "{}", bad.render());
    let mut broken = |_: &str| Ok(("util.jt".to_owned(), "fn f()\n    return nope\n".to_owned()));
    let bad = justc_core::compile_with_modules("main.jt", "import util\n", CompileOptions::DEBUG, &mut broken).unwrap_err();
    assert!(bad.render().contains("--> util.jt:2:12"), "{}", bad.render());
    assert!(justc_core::check("import util\n").iter().any(|d| d.message.contains("cannot find module `util`")));
}

#[test]
fn just_elements() {
    let src = r#"
let page = "a"

fn go(p)
    page = p

view
    window "Elements"
        group
            button "First" style: "light" -> go("a")
            button "Second" style: "frosted" -> go("b")
        split height: 200
            column width: 180
                label "Sidebar"
            column
                label "Page {page}"
        accordion "More" icon: "tune"
            label "Hidden"
        accordion "Open" open: true
            label "Shown"
"#;
    let (mut vm, _) = vm_for(src, CompileOptions::DEBUG);
    vm.run_main().unwrap();
    let nodes = vm.render_view().unwrap();
    let kinds: Vec<_> = nodes[0].children.iter().map(|n| n.kind.name()).collect();
    assert_eq!(kinds, ["group", "split", "accordion", "accordion"]);
    let text = UiNode::render_text(&nodes);
    assert!(text.contains("group\n") && text.contains("split\n") && text.contains("> ◆ More\n"), "{text}");
    assert!(!text.contains("Hidden") && text.contains("v Open\n") && text.contains("Shown"), "{text}");
    let second = nodes[0].children[0].children[1].action.unwrap();
    vm.dispatch(UiEvent::Click(second)).unwrap();
    assert!(UiNode::render_text(&vm.render_view().unwrap()).contains("Page b"));
}
