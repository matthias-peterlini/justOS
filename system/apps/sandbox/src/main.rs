//! Apps and their sandbox: who a program runs as, running a program as an
//! app, and a self-test of what the sandbox lets an app do.
//!
//! ```text
//! sandbox                         the app this program runs as
//! sandbox run <app_id> <program>  run a program as that app, in its sandbox
//! sandbox test                    check the sandbox's rules
//! ```

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use arcrt::console::color::*;
use arcrt::ipc::Port;
use arcrt::{app, eprintln, fs, println, process, Error};

arcrt::main!(main);

/// The app the test runs as.
const TEST_ID: &str = "org.arcos.prova";

fn main(args: &str) -> i32 {
    let words = arcrt::split_args(args);
    let w = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    match w(0) {
        "" | "whoami" => {
            match app::id() {
                Some(id) => println!("{id} {DIM}({}){RESET}", app::data_dir()),
                None => println!("{DIM}no app: a system program, without a sandbox{RESET}"),
            }
            0
        }
        "run" if !w(2).is_empty() => {
            if let Err(e) = app::enter(w(1), w(1)) {
                eprintln!("sandbox: cannot run as {}: {}", w(1), e.as_str());
                return 1;
            }
            let rest = words.get(3..).map(|r| r.join(" ")).unwrap_or_default();
            match process::spawn(w(2), &rest).and_then(process::wait) {
                Ok(code) => code as i32,
                Err(e) => {
                    eprintln!("sandbox: {}: {}", w(2), e.as_str());
                    1
                }
            }
        }
        "test" => test(),
        "probe" => probe(),
        // For the test: does the program run as this app?
        "is" => i32::from(app::id().as_deref() != Some(w(1))),
        _ => {
            println!("usage: sandbox | sandbox run <app_id> <program> [args] | sandbox test");
            2
        }
    }
}

fn permissions(request: &str) -> Result<String, Error> {
    let answer = Port::lookup("arc.permissions")?.call(request.as_bytes())?;
    Ok(String::from_utf8_lossy(&answer).into_owned())
}

/// Gives the test app some answers, lets `sandbox probe` run as it, then
/// forgets them.
fn test() -> i32 {
    // Answers given ahead, so the probe never waits for a dialog.
    for (perm, allow) in [("files:Musica", true), ("files:Scaricati", false), ("files:ram:", false), ("camera", false)] {
        let req = format!("{{\"set\":\"{perm}\",\"app\":\"{TEST_ID}\",\"name\":\"Prova\",\"allow\":{allow}}}");
        if !permissions(&req).is_ok_and(|a| a.contains("\"ok\":true")) {
            eprintln!("sandbox: arc.permissions did not take {perm}");
            return 1;
        }
    }
    let failed = match process::spawn("sandbox", "probe").and_then(process::wait) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("sandbox: cannot start the probe: {}", e.as_str());
            return 1;
        }
    };
    let forgot = permissions(&format!("{{\"forget\":\"{TEST_ID}\"}}")).is_ok_and(|a| a.contains("true"));
    let listed = permissions("{\"list\":true}").map(|a| a.contains(TEST_ID));
    let mut failed = failed;
    if forgot && listed == Ok(false) {
        println!("  {GREEN}ok{RESET}    answers forgotten");
    } else {
        println!("  {RED}fail{RESET}  answers forgotten {listed:?}");
        failed += 1;
    }
    if failed == 0 {
        println!("{ACCENT}sandbox: all checks passed{RESET}");
        0
    } else {
        println!("{RED}sandbox: {failed} check(s) failed{RESET}");
        1
    }
}

/// Runs as the test app and tries what an app may and may not do. The exit
/// code is the number of failures.
fn probe() -> i32 {
    let mut failed = 0;
    let mut check = |what: &str, ok: bool, detail: String| {
        if ok {
            println!("  {GREEN}ok{RESET}    {what} {DIM}{detail}{RESET}");
        } else {
            println!("  {RED}fail{RESET}  {what} {detail}");
            failed += 1;
        }
    };
    let denied = |r: Result<(), Error>| r == Err(Error::Denied);
    let entered = app::enter(TEST_ID, "Prova");
    check("enter", entered.is_ok(), format!("{entered:?}"));
    if entered.is_err() {
        return 99;
    }
    let id = app::id();
    check("app id", id.as_deref() == Some(TEST_ID), format!("{id:?}"));
    let home = format!("sys:/home/.apps/{TEST_ID}");
    let cwd = fs::current_dir();
    check("starts in its folder", cwd == home, cwd.clone());

    // Its own folder is all its.
    let own = fs::write("nota.txt", b"ciao").and_then(|_| fs::read_to_string("nota.txt"));
    check("own folder: write, read", own.as_deref() == Ok("ciao"), String::new());
    let sub = fs::make_dir("cartella").or_else(|e| if e == Error::Exists { Ok(()) } else { Err(e) });
    check("own folder: make a folder", sub.is_ok() && fs::write("cartella/a", b"1").is_ok(), format!("{sub:?}"));
    check("own folder: list", fs::list_dir(".").is_ok_and(|l| l.iter().any(|e| e.name == "nota.txt")), String::new());
    check("own folder, any case", fs::read_to_string(&format!("SYS:/HOME/.APPS/{}/NOTA.TXT", TEST_ID.to_uppercase())).is_ok(), String::new());

    // The system: programs and fonts to read, never to change; nothing else.
    check("read the programs", fs::File::open("sys:/apps/sandbox.app").is_ok(), String::new());
    check("not change them", denied(fs::write("sys:/apps/sandbox.app", b"x")), String::new());
    check("not read the settings", denied(fs::read("sys:/system/settings").map(|_| ())), String::new());
    check("not read the permissions", denied(fs::read("sys:/system/permissions.json").map(|_| ())), String::new());
    check("not list the home", denied(fs::list_dir("sys:/home").map(|_| ())), String::new());
    check("not read a file in the home", denied(fs::read("sys:/home/welcome.txt").map(|_| ())), String::new());
    check("not climb out", denied(fs::read("../../welcome.txt").map(|_| ())), String::new());
    check("not another app's folder", denied(fs::write("sys:/home/.apps/org.arcos.altra/x", b"x")), String::new());
    check("not other disks", denied(fs::list_dir("ram:/").map(|_| ())), String::new());

    // Folders of the home, as the user said.
    let music = fs::list_dir("sys:/home/Musica").map(|_| ());
    check("Musica: allowed", music.is_ok(), format!("{music:?}"));
    check("Scaricati: denied", denied(fs::write("sys:/home/Scaricati/x.txt", b"x")), String::new());
    check("what the user said", app::permission("files:Musica") == Ok(Some(true)) && app::permission("files:Scaricati") == Ok(Some(false)), String::new());
    check("not asked yet", app::permission("files:Documenti") == Ok(None), String::new());
    check("camera: denied", app::request("camera") == Ok(false), String::new());

    // Ports carry the app's name, and the table is the system's.
    check("own port", Port::create(&format!("{TEST_ID}.eco")).is_ok(), String::new());
    check("not a system port", Port::create("arc.finto").err() == Some(Error::Denied), String::new());
    check("not another app's port", Port::create("org.altra.app").err() == Some(Error::Denied), String::new());
    check("not the whole table", permissions("{\"list\":true}").is_ok_and(|a| a.contains("error")), String::new());

    // Once an app, always that app; what it starts is that app too.
    check("not become another app", app::enter("org.altra.app", "Altra") == Err(Error::Denied), String::new());
    let child = process::spawn("sandbox", &format!("is {TEST_ID}")).and_then(process::wait);
    check("programs it starts", child == Ok(0), format!("{child:?}"));
    let _ = fs::remove("cartella/a");
    let _ = fs::remove("cartella");
    let _ = fs::remove("nota.txt");
    failed
}
