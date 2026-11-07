//! The standard library: string and list builtins and the `import`able modules.

mod common;

use just_vm::Vm;
use justc_core::{compile_source, CompileOptions};
use std::cell::RefCell;
use std::rc::Rc;

fn vm_for(src: &str) -> (Vm, Rc<RefCell<Vec<String>>>) {
    let out = compile_source("test.jt", src, CompileOptions::DEBUG).unwrap_or_else(|e| panic!("{}", e.render()));
    let (host, lines) = common::StdHost::new();
    let vm = Vm::with_host(out.program, Box::new(host)).unwrap();
    (vm, lines)
}

fn run(src: &str) -> Vec<String> {
    let (mut vm, lines) = vm_for(src);
    vm.run_main().unwrap_or_else(|e| panic!("{e}"));
    let out = lines.borrow().clone();
    out
}

fn compile_errors(src: &str) -> String {
    compile_source("test.jt", src, CompileOptions::DEBUG).expect_err("expected a compile error").render()
}

#[test]
fn strings() {
    let out = run(r#"
let s = "  Hello, World  "
print(s.trim().upper(), s.trim().lower().split(", "), "a b  c".split(), s.trim_start() + "|")
print("banana".replace("a", "o"), "banana".index_of("na"), "héllo".index_of("l"), "banana".index_of("x"))
print("x".pad_start(3, "*"), "x".pad_end(3) + "|", "ab".repeat(3), "héllo".slice(1, -1), "abc".slice(-2))
print("hé".chars(), ord("A"), chr(9731), "a\nb\r\nc".lines(), "abc".starts_with("ab"), "abc".ends_with("bc"))
print(parse_int(" 42 "), parse_int("4x"), parse_float("2.5"), "abc".reversed())
"#);
    assert_eq!(
        out,
        [
            r#"HELLO, WORLD ["hello", "world"] ["a", "b", "c"] Hello, World  |"#,
            "bonono Some(2) Some(2) None",
            "**x x  | ababab éll bc",
            r#"["h", "é"] 65 ☃ ["a", "b", "c"] true true"#,
            "Some(42) None Some(2.5) cba",
        ]
    );
}

#[test]
fn lists() {
    let out = run(r#"
let xs = [5, 3, 9, 1]
print(xs.map(|x| x * 2), xs.filter(|x| x > 2), xs.reduce(0, |a, x| a + x), xs.sum(), [0.5, 1].sum())
print(xs.sorted(), xs, xs.sorted(|x| -x), min(xs), max(xs), min(2, 1), max("a", "b"))
xs.sort()
print(xs, xs.reversed(), xs.first(|x| x > 3), xs.first(), xs.last(), [].first(), [].last())
xs.reverse()
print(xs, xs.index_of(9), [true, false].any(), [true, false].all(), [1, 2].all(|x| x > 0))
print(range(0, 10, 3), range(5, 0, -2), zip([1, 2, 3], ["a", "b"]), {"b": 2, "a": 1}.items())
let words = ["pear", "fig", "apple", "kiwi"]
words.sort(|w| len(w))
print(words, [[2, "b"], [1, "z"], [2, "a"]].sorted())
let more = [1]
more.extend([2, 3])
more.extend(4..6)
print(more, more.slice(1, 3), [0].repeat(3))
more.clear()
print(more, len(more))
"#);
    assert_eq!(
        out,
        [
            "[10, 6, 18, 2] [5, 3, 9] 18 18 1.5",
            "[1, 3, 5, 9] [5, 3, 9, 1] [9, 5, 3, 1] 1 9 1 b",
            "[1, 3, 5, 9] [9, 5, 3, 1] Some(5) Some(1) Some(9) None None",
            "[9, 5, 3, 1] Some(0) true false true",
            r#"[0, 3, 6, 9] [5, 3, 1] [[1, "a"], [2, "b"]] [["a", 1], ["b", 2]]"#,
            r#"["fig", "pear", "kiwi", "apple"] [[1, "z"], [2, "a"], [2, "b"]]"#,
            "[1, 2, 3, 4, 5] [2, 3] [0, 0, 0]",
            "[] 0",
        ]
    );
}

#[test]
fn list_errors() {
    let (mut vm, _) = vm_for("print([1, \"a\"].sorted())\n");
    assert!(vm.run_main().unwrap_err().message.contains("cannot compare"));
    let (mut vm, _) = vm_for("print([1, 2].filter(|x| x))\n");
    assert!(vm.run_main().unwrap_err().message.contains("must return a bool"));
    let (mut vm, _) = vm_for("print(min([]))\n");
    assert!(vm.run_main().unwrap_err().message.contains("empty list"));
}

#[test]
fn math_and_random() {
    let out = run(r#"
import math
import random.{int, seed}
print(math.pi, math.floor(2.7), math.ceil(2.1), math.trunc(-2.7), math.round(2.567, 2), math.round(2.5), math.floor(3))
print(math.pow(2, 10), math.pow(2, -1), math.sign(-3.5), math.clamp(15, 0, 10), math.clamp(0.5, 1, 2), math.hypot(3, 4))
print(math.sqrt(16), math.abs(-2), math.max([4, 7]), math.log(8, 2), math.log10(1000), math.is_nan(math.nan), math.is_finite(math.inf))
seed(7)
let a = int(0, 1000)
random.seed(7)
print(a == random.int(0, 1000))
let counts = [0, 0, 0]
for i in 0..300
    counts[random.int(0, 3)] += 1
print(counts.all(|c| c > 50), random.float() < 1.0)
let deck = range(0, 20)
random.shuffle(deck)
print(deck.sorted() == range(0, 20), deck != range(0, 20), [1, 2, 3].contains(random.choice([1, 2, 3])))
"#);
    assert_eq!(
        out,
        [
            "3.141592653589793 2 3 -2 2.57 3 3",
            "1024 0.5 -1 10 1.0 5.0",
            "4.0 2 7 3.0 3.0 true false",
            "true",
            "true true",
            "true true true",
        ]
    );
}

#[test]
fn time() {
    let out = run(r#"
import time
let t = time.timestamp(2026, 10, 21, 9, 30, 5)
print(t, time.format(t), time.format(t, "%d/%m/%Y %a %b %j"), time.date(t)["weekday"], time.date(0)["year"])
print(time.now() > 1.7e9, time.timestamp(2024, 2, 29))
let start = time.now()
time.sleep(0.01)
print(time.now() - start >= 0.01)
"#);
    assert_eq!(out, ["1792575005 2026-10-21 09:30:05 21/10/2026 Wed Oct 294 3 1970", "true 1709164800", "true"]);
    let (mut vm, _) = vm_for("import time\nprint(time.timestamp(2025, 2, 29))\n");
    assert!(vm.run_main().unwrap_err().message.contains("is not a date"));
}

#[test]
fn files_and_paths() {
    let dir = std::env::temp_dir().join(format!("just-stdlib-{}", std::process::id()));
    let dir_text = dir.to_string_lossy().replace('\\', "\\\\");
    let out = run(&format!(
        r#"
import fs
import path
let dir = path.join("{dir_text}", "sub")
let file = path.join(dir, "notes.txt")
print(fs.mkdir(dir), fs.write(file, "one\n"), fs.append(file, "two\n"))
print(fs.read(file), fs.list(dir), fs.exists(file), fs.is_file(file), fs.is_dir(dir), fs.is_file(dir))
print(fs.copy(file, path.join(dir, "copy.txt")), fs.rename(file, path.join(dir, "moved.txt")), fs.list(dir))
print(fs.read(path.join(dir, "missing.txt")).is_err(), fs.remove(dir).is_err())
for name in fs.list(dir).unwrap()
    fs.remove(path.join(dir, name)).unwrap()
print(fs.remove(dir), fs.exists(dir))
print(path.name("a/b.tar.gz"), path.stem("a/b.txt"), path.ext("a/b.txt"), path.ext("a/b"), path.parent("a/b.txt"))
"#
    ));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        out,
        [
            "Ok(nil) Ok(nil) Ok(nil)",
            r#"Ok("one\ntwo\n") Ok(["notes.txt"]) true true true false"#,
            r#"Ok(nil) Ok(nil) Ok(["copy.txt", "moved.txt"])"#,
            "true true",
            "Ok(nil) false",
            "b.tar.gz b txt  a",
        ]
    );
}

#[test]
fn json() {
    let out = run(r#"
import json
struct P
    name
    tags = []
enum Shape
    Circle(r)
    Dot
let v = json.parse("\{\"name\": \"Just\", \"n\": [1, -2.5e1, true, null], \"u\": \"\\u00e8\\ud83d\\ude00\\n\"\}").unwrap()
print(v["name"], v["n"], v["u"])
print(json.stringify(v))
print(json.stringify(P("x", ["a", "b"]), 2))
print(json.stringify([Some(1), None, Shape.Circle(2), Shape.Dot, Ok("y"), 0.5, 1.0, {}, []]))
print(json.parse("[1,"), json.parse("\{\"a\" 1\}"), json.parse("[1] x"))
print(json.parse(json.stringify({"k": [1, "two", 3.5]})).unwrap()["k"])
"#);
    assert_eq!(
        out,
        [
            "Just [1, -25.0, true, nil] è😀\n",
            r#"{"n":[1,-25.0,true,null],"name":"Just","u":"è😀\n"}"#,
            "{\n  \"name\": \"x\",\n  \"tags\": [\n    \"a\",\n    \"b\"\n  ]\n}",
            r#"[1,null,{"Circle":2},"Dot",{"Ok":"y"},0.5,1.0,{},[]]"#,
            r#"Err("invalid JSON at line 1, column 4: unexpected end of input") Err("invalid JSON at line 1, column 6: expected `:`") Err("invalid JSON at line 1, column 5: unexpected text after the JSON value")"#,
            r#"[1, "two", 3.5]"#,
        ]
    );
    let (mut vm, _) = vm_for("import json\nlet l = [1]\nl.push(l)\nprint(json.stringify(l))\n");
    assert!(vm.run_main().unwrap_err().message.contains("nested too deeply"));
}

#[test]
fn os() {
    let (mut vm, lines) = vm_for(
        r#"
import os
print(os.args(), os.name == "windows" or os.name == "linux" or os.name == "macos", len(os.sep))
print(os.env("JUST_STDLIB_TEST_UNSET").is_none(), os.cwd() != "")
print(os.run("just-no-such-program").is_err())
os.exit(3)
print("not reached")
"#,
    );
    vm.set_args(vec!["one".into(), "two words".into()]);
    let err = vm.run_main().unwrap_err();
    assert_eq!(vm.exit_code(), Some(3), "{err}");
    assert_eq!(lines.borrow().as_slice(), [r#"["one", "two words"] true 1"#, "true true", "true"]);
}

#[test]
fn read_line_without_console() {
    // Hosts without a console have no input.
    assert_eq!(run("print(read_line(\"name? \"))\n"), ["None"]);
}

/// Answers every request with its own method, URL, body and headers.
struct EchoHttp(Rc<RefCell<Vec<String>>>);

impl just_vm::Host for EchoHttp {
    fn print(&mut self, text: &str) {
        self.0.borrow_mut().push(text.to_owned());
    }

    fn http(&mut self, r: &just_vm::HttpRequest<'_>) -> Result<just_vm::HttpResponse, String> {
        if r.url.contains(":1/") {
            return Err("connection refused".into());
        }
        let headers: Vec<String> = r.headers.iter().map(|(k, v)| format!("{k}={v}")).collect();
        Ok(just_vm::HttpResponse {
            status: if r.method == "DELETE" { 404 } else { 200 },
            body: format!("{} {} {}", r.method, r.url, r.body.unwrap_or("-")),
            headers: vec![("x-test".into(), headers.join(","))],
        })
    }
}

#[test]
fn http() {
    let src = r#"
import http
let r = http.post("http://example/p", {"a": 1}, {"X-Key": "k"}).unwrap()
print(r["status"], r["body"], r["headers"]["x-test"])
print(http.request("delete", "http://example/").unwrap()["status"], http.get("http://127.0.0.1:1/").is_err())
"#;
    let out = compile_source("test.jt", src, CompileOptions::DEBUG).unwrap();
    let lines = Rc::new(RefCell::new(Vec::new()));
    let mut vm = Vm::with_host(out.program, Box::new(EchoHttp(Rc::clone(&lines)))).unwrap();
    vm.run_main().unwrap();
    assert_eq!(lines.borrow().as_slice(), [r#"200 POST http://example/p {"a":1} X-Key=k,Content-Type=application/json"#, "404 true"]);
    // Without network support in the host, requests are errors.
    assert_eq!(run("import http\nprint(http.get(\"http://example/\"))\n"), [r#"Err("this runtime has no network support")"#]);
}

#[test]
fn std_module_diagnostics() {
    let errors = compile_errors(
        r#"
import math.{sine}
import json
print(json.parze("1"))
print(json.stringify())
json.parse = 1
print(fs.read("x"))
print(math)
"#,
    );
    for expected in [
        "module `math` has no `sine`",
        "a similar name exists: `sin`",
        "module `json` has no `parze`",
        "`json.stringify` called with 0 argument(s)",
        "`json.parse` belongs to the standard library and cannot be changed",
        "the standard library module `fs` is not imported",
        "add `import fs` at the top of the file",
        "`math` is a module, not a value",
    ] {
        assert!(errors.contains(expected), "missing {expected:?} in:\n{errors}");
    }
    // Imported names and module functions used as values.
    let out = run(r#"
import math.{pi, floor}
import json
let parse = json.parse
print(floor(pi), parse("[2]").unwrap(), [1.5, 2.5].map(math_floor))
fn math_floor(x)
    return floor(x)
"#);
    assert_eq!(out, ["3 [2] [1, 2]"]);
}
