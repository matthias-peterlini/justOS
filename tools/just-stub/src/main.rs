//! just-stub: the runtime of standalone JustScript programs.
//!
//! `justc build -t windows` appends a `.jpk` package to this program (see
//! `jpk-bundler`) and the result is a Windows `.exe` that needs nothing
//! else: started, it finds the package at the end of its own file and runs
//! it, as `just` does on justOS. A program with a view shows its window as
//! text; in a console its buttons, checkboxes and inputs are numbered and
//! work from the keyboard.
//!
//! Without a package the stub runs the one it is given, or the one inside
//! a program it made: `just-stub app.jpk [ARGUMENTS...]`.

mod host;

use just_vm::ui::{Attr, UiKind};
use just_vm::{ActionId, RuntimeError, UiEvent, UiNode, UiValue, Vm};
use justc_core::jpk::Jpk;
use std::io::{IsTerminal, Write};

/// Exit codes of JustScript's runtimes (BSD `EX_SOFTWARE`, `EX_DATAERR`).
const EXIT_RUNTIME_ERROR: i32 = 70;
const EXIT_BAD_PACKAGE: i32 = 65;

const HELP: &str = "\
just-stub - the runtime of standalone JustScript programs

  just-stub <FILE.jpk|FILE.exe> [ARGUMENTS...]   run a package

justc build -t windows appends a package to this runtime: the .exe it makes
runs that package by itself.";

fn main() {
    let code = run();
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}

fn run() -> i32 {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let own = std::env::current_exe().and_then(std::fs::read).unwrap_or_default();
    let bytes = match jpk_bundler::payload(&own) {
        Ok(package) => package.to_vec(),
        Err(jpk_bundler::Error::NoPackage) => {
            // The bare runtime: the package comes from the command line.
            match args.first().map(String::as_str) {
                None | Some("-h" | "--help") => {
                    println!("{HELP}");
                    return if args.is_empty() { 2 } else { 0 };
                }
                Some("-V" | "--version") => {
                    println!("just-stub {}", env!("CARGO_PKG_VERSION"));
                    return 0;
                }
                Some(_) => {}
            }
            let path = args.remove(0);
            let file = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("{path}: {e}");
                    return EXIT_BAD_PACKAGE;
                }
            };
            if jpk_bundler::format(&file).is_none() {
                file
            } else {
                match jpk_bundler::payload(&file) {
                    Ok(package) => package.to_vec(),
                    Err(e) => {
                        eprintln!("{path}: {e}");
                        return EXIT_BAD_PACKAGE;
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("{}: {e}", std::env::args().next().unwrap_or_default());
            return EXIT_BAD_PACKAGE;
        }
    };
    let package = match Jpk::from_bytes(&bytes) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return EXIT_BAD_PACKAGE;
        }
    };
    let name = package.manifest.get("name").unwrap_or("JustScript").to_owned();
    let title = package.manifest.get("title").unwrap_or(&name).to_owned();
    let app_id = package.manifest.get("app_id").map_or_else(|| justc_core::jpk::app_id(&name), str::to_owned);
    let mut vm = match Vm::with_host(package.program, Box::new(host::StdHost::new())) {
        Ok(vm) => vm,
        Err(e) => {
            eprintln!("{name}: {e}");
            return EXIT_BAD_PACKAGE;
        }
    };
    vm.set_args(args);
    vm.set_app(&title, &app_id);
    let result = vm.run_main().and_then(|()| if vm.has_view() { show(&mut vm, &title) } else { Ok(()) });
    match result {
        Ok(()) => 0,
        Err(e) => match vm.exit_code() {
            Some(code) => code,
            None => {
                eprintln!("{e}");
                EXIT_RUNTIME_ERROR
            }
        },
    }
}

/// What the keyboard can do with an element of the view.
enum Control {
    Event(UiEvent),
    /// An input: asks for the new text.
    Text(ActionId),
}

fn control(n: &UiNode) -> Option<(String, Control)> {
    let action = n.action?;
    let caption = || n.text_attr(Attr::Text).unwrap_or(&n.text).to_owned();
    match n.kind {
        UiKind::Button => {
            // The console has no icon font: the icon's name tells buttons apart.
            let icon = n.icon().and_then(|c| justc_core::ui::icon_names().find(|&name| justc_core::ui::icon(name) == Some(c)));
            let label = match icon {
                Some(name) if n.text.is_empty() => name.to_owned(),
                Some(name) => format!("{} ({name})", n.text),
                None => n.text.clone(),
            };
            Some((format!("press  {label}"), Control::Event(UiEvent::Click(action))))
        }
        UiKind::Checkbox | UiKind::Switch => {
            let on = n.value.as_ref().and_then(UiValue::as_bool) == Some(true);
            let verb = if on { "turn off" } else { "turn on" };
            Some((format!("{verb} {}", caption()), Control::Event(UiEvent::Change(action, UiValue::Bool(!on)))))
        }
        UiKind::Input => {
            let hint = n.text_attr(Attr::Placeholder).map(str::to_owned).unwrap_or_else(|| n.text.clone());
            Some((format!("type   {hint}"), Control::Text(action)))
        }
        _ => None,
    }
}

fn read_line() -> Option<String> {
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_owned()),
    }
}

/// The window as text; at a console, a loop over its controls until the
/// user closes it.
fn show(vm: &mut Vm, title: &str) -> Result<(), RuntimeError> {
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    loop {
        let nodes = vm.render_view()?;
        print!("{}", UiNode::render_text(&nodes));
        let controls: Vec<(String, Control)> = nodes.iter().flat_map(UiNode::iter).filter_map(control).collect();
        if !interactive || controls.is_empty() {
            return Ok(());
        }
        for (i, (label, _)) in controls.iter().enumerate() {
            println!("  {:>2}  {label}", i + 1);
        }
        print!("{title}: the number of a control, or Enter to close: ");
        let Some(line) = read_line() else { return Ok(()) };
        let line = line.trim();
        if line.is_empty() {
            return Ok(());
        }
        match line.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|i| controls.get(i)) {
            Some((_, Control::Event(event))) => vm.dispatch(event.clone())?,
            Some((_, Control::Text(action))) => {
                print!("text: ");
                if let Some(text) = read_line() {
                    vm.dispatch(UiEvent::Change(*action, UiValue::Text(text)))?;
                }
            }
            None => println!("there is no control {line}"),
        }
        println!();
    }
}
