//! The programs in `tests/java`, built into DEX files by `tests/build.sh`,
//! must print what a real JVM prints (`tests/*.out`).

use super::*;
use alloc::rc::Rc;
use alloc::vec;
use core::cell::RefCell;
use std::string::String;

#[derive(Default)]
struct Io {
    out: String,
    err: String,
    input: Vec<String>,
    clock: u64,
}

#[derive(Clone, Default)]
struct TestHost(Rc<RefCell<Io>>);

impl Host for TestHost {
    fn write(&mut self, s: &str, err: bool) {
        let mut io = self.0.borrow_mut();
        if err {
            io.err.push_str(s);
        } else {
            io.out.push_str(s);
        }
    }
    fn read_line(&mut self) -> Option<String> {
        let mut io = self.0.borrow_mut();
        if io.input.is_empty() {
            None
        } else {
            Some(io.input.remove(0))
        }
    }
    fn time_ms(&mut self) -> u64 {
        1_700_000_000_000
    }
    fn uptime_ms(&mut self) -> u64 {
        self.0.borrow().clock
    }
    fn sleep(&mut self, ms: u64) {
        self.0.borrow_mut().clock += ms;
    }
}

fn vm(dex: &[u8], input: &str) -> (Vm, TestHost) {
    let host = TestHost::default();
    host.0.borrow_mut().input = input.lines().map(String::from).collect();
    let dexes = load(dex).unwrap();
    (Vm::new(dexes, Box::new(host.clone())), host)
}

#[test]
fn reads_the_tables() {
    let d = &load(include_bytes!("../tests/Basics.dex")).unwrap()[0];
    assert!(d.types.iter().any(|t| &**t == "LBasics$Circle;"));
    assert!(d.strings.iter().any(|s| &**s == "metà"));
    assert!(d.protos.iter().any(|p| &*p.sig == "([Ljava/lang/String;)V"));
}

#[test]
fn basics_print_what_java_prints() {
    let (mut vm, host) = vm(include_bytes!("../tests/Basics.dex"), "");
    let main = vm.main_class().unwrap();
    assert_eq!(&*main, "LBasics;");
    let code = vm.run_main(&main, &[]);
    let io = host.0.borrow();
    assert_eq!(io.err, "");
    let want = include_str!("../tests/Basics.out");
    for (i, (got, want)) in io.out.lines().zip(want.lines()).enumerate() {
        assert_eq!(got, want, "line {}", i + 1);
    }
    assert_eq!(io.out, want);
    assert_eq!(code, 0);
}

#[test]
fn input_from_the_keyboard() {
    let (mut vm, host) = vm(include_bytes!("../tests/Input.dex"), include_str!("../tests/Input.in"));
    let code = vm.run_main("LInput;", &[]);
    let io = host.0.borrow();
    let out = format!("{}exit {code}\n", io.out);
    assert_eq!(out, include_str!("../tests/Input.out"));
}

#[test]
fn uncaught_exceptions_end_the_program() {
    let (mut vm, host) = vm(include_bytes!("../tests/Basics.dex"), "");
    let ex = vm.exception("Ljava/lang/IllegalStateException;", "rotto");
    assert_eq!(vm.uncaught(Throw::Ex(ex)), 1);
    assert_eq!(host.0.borrow().err, "Exception in thread \"main\" java.lang.IllegalStateException: rotto\n");
}

fn find_view(root: &Ref, id: i32) -> Option<Ref> {
    let v = android::view(root)?;
    if v.id == id {
        return Some(root.clone());
    }
    v.children.iter().find_map(|c| find_view(c, id))
}

fn text_of(r: &Ref) -> String {
    android::view(r).unwrap().text.clone()
}

#[test]
fn an_activity_built_in_code() {
    let (mut vm, host) = vm(include_bytes!("../tests/Counter.dex"), "");
    let class = vm.activity_class().unwrap();
    assert_eq!(&*class, "Larcos/demo/MainActivity;");
    vm.start_activity(&class).unwrap();
    assert_eq!(vm.ui.title.as_deref(), Some("Contatore"));
    assert_eq!(host.0.borrow().err, "D/Contatore: creato\n");
    let root = vm.ui.content.clone().unwrap();
    {
        let r = android::view(&root).unwrap();
        assert!(r.vertical);
        assert_eq!(r.children.len(), 6);
        let title = android::view(&r.children[0]).unwrap();
        assert_eq!(title.text, "Ciao da Android!");
        assert_eq!(title.text_size, 24.0);
        assert_eq!(title.text_color, Some(0xFF62_00EE));
    }
    let label = find_view(&root, 1).unwrap();
    let plus = find_view(&root, 2).unwrap();
    vm.click(&plus).unwrap();
    assert_eq!(text_of(&label), "Premuto 1 volta");
    for _ in 0..4 {
        vm.click(&plus).unwrap();
    }
    assert_eq!(text_of(&label), "Premuto 5 volte");
    assert_eq!(vm.ui.toasts, vec![("Già 5!".into(), 2000)]);

    let name = find_view(&root, 3).unwrap();
    let hello = find_view(&root, 4).unwrap();
    vm.click(&hello).unwrap();
    assert_eq!(text_of(&label), "Scrivi un nome");
    vm.type_text(&name, "Mattiaa", false);
    vm.type_text(&name, "", true);
    vm.click(&hello).unwrap();
    assert_eq!(text_of(&label), "Ciao Mattia!");

    let check = find_view(&root, 5).unwrap();
    vm.click(&check).unwrap();
    assert!(android::view(&check).unwrap().checked);

    assert_eq!(vm.next_timer(), Some(10));
    vm.run_timers().unwrap();
    assert_eq!(vm.ui.title.as_deref(), Some("Contatore"));
    host.0.borrow_mut().clock = 10;
    vm.run_timers().unwrap();
    assert_eq!(vm.ui.title.as_deref(), Some("Contatore (1)"));
    assert_eq!(vm.next_timer(), None);
    vm.stop_activity().unwrap();
}

#[test]
fn regexes() {
    use text::Regex;
    let r = Regex::new("\\s*,\\s*").unwrap();
    assert_eq!(r.split("a , b,c ,d", 0), vec!["a", "b", "c", "d"]);
    assert!(Regex::new("[A-Z][a-z]+(-[A-Z][a-z]+)?").unwrap().matches("Alto-Adige"));
    assert!(!Regex::new("a+b").unwrap().matches("aaac"));
    assert_eq!(Regex::new("o+").unwrap().replace("foo boo", "0", true), "f0 b0");
    assert_eq!(Regex::new("").unwrap().split("abc", 0), vec!["a", "b", "c"]);
    assert_eq!(Regex::new(",").unwrap().split("a,,b,,", 0), vec!["a", "", "b"]);
}

#[test]
fn numbers_as_java_writes_them() {
    assert_eq!(text::double(1e7), "1.0E7");
    assert_eq!(text::double(123456.789), "123456.789");
    assert_eq!(text::double(-0.00012), "-1.2E-4");
    assert_eq!(text::float(3.0), "3.0");
    assert_eq!(text::format("%08.3f|%+d|%s|%-4d|", &[text::Arg::Float(-3.14159), text::Arg::Int(5), text::Arg::Text("x".into(), false), text::Arg::Int(7)]).unwrap(), "-003.142|+5|x|7   |");
}
