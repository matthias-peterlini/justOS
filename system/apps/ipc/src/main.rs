//! Messages between programs: a small echo service, a client for any port,
//! and a self-test.
//!
//! ```text
//! ipc serve <name>          answer calls on a named port (in capitals)
//! ipc call <name> <text>    send a request and print the answer
//! ipc notify <title> <text> show a notification through just.notify
//! ipc test                  start a service, talk to it, check everything
//! ```

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use justrt::console::color::*;
use justrt::ipc::{self, Port};
use justrt::{eprintln, println, Error};

justrt::main!(main);

fn main(args: &str) -> i32 {
    let words = justrt::split_args(args);
    let w = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    let rest = |from: usize| words.get(from..).map(|r| r.join(" ")).unwrap_or_default();
    let r = match w(0) {
        "serve" if !w(1).is_empty() => serve(w(1)),
        "call" if !w(1).is_empty() => Port::lookup(w(1)).and_then(|p| p.call(rest(2).as_bytes())).map(|a| println!("{}", String::from_utf8_lossy(&a))),
        "notify" if !w(1).is_empty() => notify(w(1), &rest(2)).map(|a| println!("{a}")),
        "test" => return test(),
        _ => {
            println!("usage: ipc serve <name> | call <name> <text> | notify <title> <text> | test");
            return 2;
        }
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ipc: {}", e.as_str());
            1
        }
    }
}

/// Answers calls in capitals, saying who asked; counts plain messages.
/// `stop` ends it.
fn serve(name: &str) -> justrt::Result<()> {
    let port = Port::create(name)?;
    let mut count = 0;
    loop {
        let m = port.recv(None)?;
        let who = ipc::process_name(m.sender).unwrap_or_default();
        match m.text() {
            "stop" => {
                m.reply(b"bye")?;
                return Ok(());
            }
            "count?" => m.reply(format!("{count}").as_bytes())?,
            t if m.reply.is_none() => {
                let _ = t;
                count += 1;
            }
            t => m.reply(format!("{} (from {who}, pid {})", t.to_uppercase(), m.sender).as_bytes())?,
        }
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn notify(title: &str, text: &str) -> justrt::Result<String> {
    let msg = format!("{{\"title\":{},\"text\":{}}}", json_string(title), json_string(text));
    let answer = Port::lookup("just.notify")?.call(msg.as_bytes())?;
    Ok(String::from_utf8_lossy(&answer).into_owned())
}

/// Starts `ipc serve` in another process and checks what ports promise.
fn test() -> i32 {
    const NAME: &str = "just.test.echo";
    let mut failed = 0;
    let mut check = |what: &str, ok: bool, detail: String| {
        if ok {
            println!("  {GREEN}ok{RESET}    {what} {DIM}{detail}{RESET}");
        } else {
            println!("  {RED}fail{RESET}  {what} {detail}");
            failed += 1;
        }
    };
    let pid = match justrt::process::spawn("ipc", &format!("serve {NAME}")) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ipc: cannot start the service: {}", e.as_str());
            return 1;
        }
    };
    // The service needs a moment to make its port.
    let mut port = Err(Error::NotFound);
    for _ in 0..150 {
        port = Port::lookup(NAME);
        if port.is_ok() {
            break;
        }
        justrt::time::sleep_ms(20);
    }
    let port = match port {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ipc: the service did not appear: {}", e.as_str());
            return 1;
        }
    };
    check("lookup", true, format!("{NAME} (service pid {pid})"));
    let answer = port.call(b"ciao dalle porte").map(|a| String::from_utf8_lossy(&a).into_owned());
    check("call", answer.as_ref().is_ok_and(|a| a.starts_with("CIAO DALLE PORTE (from ipc, pid ")), format!("{answer:?}"));
    for _ in 0..3 {
        let _ = port.send(b"tick");
    }
    let count = port.call(b"count?").map(|a| String::from_utf8_lossy(&a).into_owned());
    check("send, in order", count.as_deref() == Ok("3"), format!("{count:?}"));
    check("name taken", Port::create(NAME).err() == Some(Error::Exists), String::new());
    check("bad name", Port::create("Not A Name").err() == Some(Error::InvalidName), String::new());
    let big = alloc::vec![0u8; justrt::sys::PORT_MSG_MAX as usize + 1];
    check("too large", port.send(&big).err() == Some(Error::TooLarge), String::new());
    let bye = port.call(b"stop").map(|a| String::from_utf8_lossy(&a).into_owned());
    check("stop", bye.as_deref() == Ok("bye"), format!("{bye:?}"));
    let code = justrt::process::wait(pid);
    check("service ended", code == Ok(0), format!("{code:?}"));
    check("gone after its owner", port.call(b"hello?").err() == Some(Error::Closed), String::new());
    check("unlisted after its owner", Port::lookup(NAME).err() == Some(Error::NotFound), String::new());
    // A private port: closed for its receiver once nobody can send to it.
    match Port::private() {
        Ok(p) => check("private port, nobody sending", p.recv(Some(100)).err() == Some(Error::Closed), String::new()),
        Err(e) => check("private port", false, String::from(e.as_str())),
    }
    let n = notify("ipc", "Messaggio di prova dalle porte");
    check("just.notify", n.as_deref() == Ok("{\"ok\":true}"), format!("{n:?}"));
    if failed == 0 {
        println!("{ACCENT}ipc: all checks passed{RESET}");
        0
    } else {
        println!("{RED}ipc: {failed} check(s) failed{RESET}");
        1
    }
}
