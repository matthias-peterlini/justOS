//! justjs: a small JavaScript interpreter for the JustOS Browser.
//!
//! It walks the syntax tree directly. It covers most of ES2020 that real
//! pages use (classes, arrows, destructuring, spread, template strings,
//! promises, async/await, Map/Set, regular expressions, JSON, Date), but
//! not generators or modules. `await` doesn't suspend: it runs the queued
//! jobs and the timers the promise waits on until it settles. Scripts run with a step budget and a call
//! depth limit, so a page can't hang its thread or overflow its stack.
//!
//! The embedder hooks into host objects through [`Kind::Host`] and the
//! `host_get`/`host_set` callbacks, and drives timers with
//! [`Interp::run_timers`].

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod array;
pub mod ast;
pub mod builtins;
pub mod date;
pub mod interp;
pub mod json;
pub mod lexer;
pub mod parser;
pub mod regex;
pub mod string;
pub mod value;

pub use interp::{arg, Ctl, Interp, R};
pub use value::{Key, Kind, NativeFn, ObjRef, Prop, Slot, Value};

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    fn out(src: &str) -> String {
        let mut it = Interp::new();
        let _ = it.run(src);
        it.console.join("\n")
    }

    fn eq(src: &str, want: &str) {
        assert_eq!(out(src), want, "script: {src}");
    }

    #[test]
    fn basics() {
        eq("console.log(1 + 2, 'a' + 1, 7 / 2, 2 ** 10)", "3 a1 3.5 1024");
        eq("let x = 0; for (let i = 0; i < 10; i++) x += i; console.log(x)", "45");
        eq("var s = ''; for (const k in {a: 1, b: 2}) s += k; console.log(s)", "ab");
        eq("console.log(typeof null, typeof undefined, typeof 1, typeof 'x', typeof {}, typeof (() => 1))", "object undefined number string object function");
        eq("console.log(0.1 + 0.2, 1e21, 1 / 3, -0 === 0, NaN === NaN)", "0.30000000000000004 1e+21 0.3333333333333333 true false");
        eq("console.log([1, 2, 3].map(x => x * 2).join('-'))", "2-4-6");
        eq("console.log('5' * '2', '5' + 2, true + 1, [] + [], [1] == 1, null == undefined, null === undefined)", "10 52 2  true true false");
    }

    #[test]
    fn functions_and_closures() {
        eq("function f(n) { return n < 2 ? n : f(n - 1) + f(n - 2) } console.log(f(20))", "6765");
        eq("const mk = () => { let c = 0; return () => ++c }; const g = mk(); g(); g(); console.log(g())", "3");
        eq("function f(a, b = a * 2, ...r) { return [a, b, r.length, arguments.length] } console.log(f(1).join(), f(1, 5, 6, 7).join())", "1,2,0,1 1,5,2,4");
        eq("console.log(hoisted()); function hoisted() { return 'ok' }", "ok");
        eq("const o = { n: 'o', f() { return this.n } }; const b = o.f.bind({ n: 'b' }); console.log(o.f(), b(), o.f.call({ n: 'c' }))", "o b c");
    }

    #[test]
    fn destructuring_and_spread() {
        eq("const { a, b: [c, d = 4], ...rest } = { a: 1, b: [3], e: 5, f: 6 }; console.log(a, c, d, Object.keys(rest).join())", "1 3 4 e,f");
        eq("let x = 1, y = 2; [x, y] = [y, x]; console.log(x, y)", "2 1");
        eq("console.log(Math.max(...[1, 9, 3]), [...'abc'].length, {...{a: 1}, b: 2}.b)", "9 3 2");
    }

    #[test]
    fn classes() {
        eq(
            "class A { #p = 1; static s = 'S'; constructor(x) { this.x = x } get dbl() { return this.x * 2 } m() { return 'A' + this.#p } }
             class B extends A { constructor() { super(5) } m() { return 'B' + super.m() } }
             const b = new B(); console.log(b.x, b.dbl, b.m(), B.s, b instanceof A)",
            "5 10 BA1 S true",
        );
        eq("class E extends Error { constructor(m) { super(m); this.name = 'E' } } try { throw new E('no') } catch (e) { console.log(e.name, e.message, e instanceof Error) }", "E no true");
    }

    #[test]
    fn errors() {
        eq("try { null.x } catch (e) { console.log(e instanceof TypeError) } finally { console.log('f') }", "true\nf");
        eq("undefinedVar", "Uncaught ReferenceError: undefinedVar is not defined");
        eq("while (true) {}", "script interrotto: ci mette troppo");
        eq("function r() { return r() } try { r() } catch (e) { console.log(e instanceof RangeError) }", "true");
    }

    #[test]
    fn strings_and_regex() {
        eq("console.log('Ciao Mondo'.toUpperCase(), 'abc'.at(-1), 'a,b,,c'.split(','). length, ' x '.trim() + '|')", "CIAO MONDO c 4 x|");
        eq("console.log('2024-01-05'.replace(/(\\d+)-(\\d+)-(\\d+)/, '$3/$2/$1'))", "05/01/2024");
        eq("console.log('a1b22c333'.match(/\\d+/g).join(' '), /^h.llo$/i.test('HELLO'))", "1 22 333 true");
        eq("const n = 3; console.log(`n=${n * 2}, ${'x'.repeat(n)}`)", "n=6, xxx");
        eq("console.log('x-y_z'.replace(/[-_](.)/g, (m, c) => c.toUpperCase()))", "xYZ");
    }

    #[test]
    fn collections_and_json() {
        eq("const m = new Map([[1, 'a']]); m.set('k', 2); console.log(m.size, m.get(1), [...m.keys()].join())", "2 a 1,k");
        eq("const s = new Set([1, 2, 2, 3]); console.log(s.size, s.has(2))", "3 true");
        eq("console.log(JSON.stringify({ a: [1, 'x', null, true], b: { c: undefined }, d: 1.5 }))", r#"{"a":[1,"x",null,true],"b":{},"d":1.5}"#);
        eq("console.log(JSON.stringify([1, { a: 2 }], null, 2))", "[\n  1,\n  {\n    \"a\": 2\n  }\n]");
        eq(r#"const o = JSON.parse('{"x": [1, 2, {"y": "\\u00e8"}], "n": -1.5e2}'); console.log(o.x[2].y, o.n)"#, "è -150");
        eq("try { JSON.parse('{bad') } catch (e) { console.log(e.name) }", "SyntaxError");
        eq("const a = {}; a.a = a; try { JSON.stringify(a) } catch (e) { console.log(e.name) }", "TypeError");
        eq("console.log([3, 1, 10, 2].sort().join(), [3, 1, 10, 2].sort((a, b) => a - b).join())", "1,10,2,3 1,2,3,10");
    }

    #[test]
    fn dates() {
        eq("const d = new Date(Date.UTC(2024, 0, 5, 10, 20)); console.log(d.getFullYear(), d.getMonth(), d.getDate(), d.getDay(), d.toISOString())", "2024 0 5 5 2024-01-05T10:20:00.000Z");
        eq("const d = new Date('2024-02-28'); d.setDate(d.getDate() + 2); console.log(d.toISOString().slice(0, 10), JSON.stringify({ d }))", r#"2024-03-01 {"d":"2024-03-01T00:00:00.000Z"}"#);
        eq("console.log(new Date('nope').getTime(), String(new Date(NaN)))", "NaN Invalid Date");
        eq("console.log(new Date(2020, 11, 31).toLocaleDateString())", "31/12/2020");
    }

    #[test]
    fn promises_and_timers() {
        eq("Promise.resolve(1).then(v => console.log('then', v)); console.log('sync')", "sync\nthen 1");
        // `await` settles in place, running the timers it waits on early.
        eq("(async () => { const v = await new Promise(r => setTimeout(() => r(7), 10)); console.log('got', v) })(); console.log('next')", "got 7\nnext");
        let mut it = Interp::new();
        let _ = it.run("setTimeout(() => console.log('t2'), 20); setTimeout(() => console.log('t1'), 10); Promise.all([1, Promise.resolve(2)]).then(v => console.log(v.join('+')))");
        assert_eq!(it.console.join("\n"), "1+2");
        it.run_timers(15.0);
        it.run_timers(25.0);
        assert_eq!(it.console.join("\n"), "1+2\nt1\nt2");
    }

    #[test]
    fn everyday_code() {
        eq("const fs = []; for (let i = 0; i < 3; i++) fs.push(() => i); console.log(fs.map(f => f()).join())", "0,1,2");
        eq("console.log(Object.entries({ a: 1, b: 2 }).map(([k, v]) => k + v).join(), Array.from({ length: 3 }, (_, i) => i * i).join())", "a1,b2 0,1,4");
        eq("console.log([1, 2, 3, 4].reduce((a, b) => a + b, 0), [5, 6].find(x => x > 5), [1, 2].includes(2), [[1], [2, [3]]].flat(Infinity).join())", "10 6 true 1,2,3");
        eq("console.log('7'.padStart(3, '0'), (3.14159).toFixed(2), (255).toString(16), parseInt('42px'), Number('0x10'))", "007 3.14 ff 42 16");
        eq("const o = { a: { b: null } }; console.log(o?.a?.b?.c, o.x ?? 'def', o.a.b ?? 0)", "undefined def 0");
        eq("function t(s, ...v) { return s.raw.join('|') + v.join() } console.log(t`a${1}b${2}c`)", "a|b|c1,2");
        eq("const it = { *[Symbol.iterator]() {} }", "SyntaxError: riga 1: i generatori non sono supportati (trovato '[')");
        eq("const r = { from: 1, to: 3, [Symbol.iterator]() { let c = this.from, t = this.to; return { next: () => c <= t ? { value: c++, done: false } : { value: undefined, done: true } } } }; console.log([...r].join())", "1,2,3");
        eq("switch (3) { case 1: console.log(1); case 3: console.log(3); case 4: console.log(4); break; default: console.log('d') }", "3\n4");
        eq("outer: for (const a of [1, 2]) { for (const b of [1, 2]) { if (b === 2) continue outer; console.log(a, b) } }", "1 1\n2 1");
        eq("console.log(String([1, [2, 3]]), String({}), `${[1, 2]}`, [1, 2] + '')", "1,2,3 [object Object] 1,2 1,2");
        eq("console.log({ a: 1, b: [1, 'x'], c: { d: null } })", "{ a: 1, b: [1, 'x'], c: { d: null } }");
        eq("const { log } = console; log(Object.freeze({ a: 1 }).a, Object.assign({}, { b: 2 }).b)", "1 2");
        eq("console.log(new Intl.NumberFormat('it-IT').format(1234567.5))", "1.234.567,5");
    }

    #[test]
    fn syntax_errors_are_reported() {
        let mut it = Interp::new();
        let e = it.run("let = ;").err().expect("syntax error");
        assert!(e.starts_with("SyntaxError"), "{e}");
    }

    #[test]
    fn deep_recursion_fits_a_small_stack() {
        // The browser runs scripts on a 2 MiB thread; leave a margin.
        let h = std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn(|| {
                out("function r(n) { return n ? 1 + r(n - 1) : 0 } try { console.log(r(1e6)) } catch (e) { console.log(e.name) }
                     let s = ''; for (let i = 0; i < 300; i++) s += '['; try { JSON.parse(s) } catch (e) { console.log(e.name) }
                     try { eval('('.repeat(500) + '1' + ')'.repeat(500)) } catch (e) { console.log(e.name) }")
            })
            .unwrap();
        assert_eq!(h.join().unwrap(), "RangeError\nSyntaxError\nSyntaxError");
    }
}
