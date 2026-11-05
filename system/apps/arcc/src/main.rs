//! `arcc`: ArcScript on JustOS. Compiles `.as` programs and projects into
//! `.arc` packages and runs both, with the commands of ArcScript's `arcc`
//! (and `arc-runner --info`). The shell starts `.arc` and `.as` files
//! through it.

#![no_std]
#![no_main]

extern crate alloc;

mod host;
mod project;

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use arc_vm::{UiNode, Vm};
use arcc_core::package::ArcPackage;
use arcc_core::CompileOptions;
use justrt::console::color::*;
use justrt::{eprintln, fs, print, println};
use project::Input;

justrt::main!(main);

const HELP: &str = "\
arcc - ArcScript on JustOS

  arcc run <PATH> [ARGUMENTS...]   compile and run (also a .arc package)
  arcc <FILE.as|FILE.arc> [ARGS]   the same
  arcc build <PATH> [-r] [-o OUT]  compile to a .arc package
  arcc check <PATH>                report errors and warnings only
  arcc disasm <PATH>               the bytecode of a .as or .arc
  arcc info <FILE.arc>             manifest and contents of a package
  arcc new <FOLDER>                a new project

PATH is a .as file or a project folder with an Arc.toml (default: the
current folder). -r builds without debug information. Arguments after
PATH (or after --) are the program's: os.args().
Programs with a view are shown as text here; ArcStudio, in the desktop,
shows them live.";

/// Exit codes of ArcScript's runtimes (BSD `EX_SOFTWARE`, `EX_DATAERR`).
const EXIT_RUNTIME_ERROR: i32 = 70;
const EXIT_BAD_PACKAGE: i32 = 65;

struct Args {
    command: String,
    path: Option<String>,
    release: bool,
    output: Option<String>,
    program_args: Vec<String>,
}

fn parse_args(words: Vec<String>) -> Result<Args, String> {
    let mut it = words.into_iter();
    let (mut command, mut path, mut release, mut output) = (None, None, false, None);
    let mut program_args = Vec::new();
    while let Some(arg) = it.next() {
        // Everything after the program is the program's own.
        let running = command.as_deref().is_some_and(|c| c == "run" || is_program(c));
        if arg == "--" || (running && path.is_some()) {
            if arg != "--" {
                program_args.push(arg);
            }
            program_args.extend(it.by_ref());
            break;
        }
        match arg.as_str() {
            "-h" | "--help" => command = Some("help".to_owned()),
            "-V" | "--version" => command = Some("version".to_owned()),
            "-r" | "--release" => release = true,
            "-o" | "--output" => output = Some(it.next().ok_or("missing value for --output")?),
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option `{s}`")),
            _ if command.is_none() => command = Some(arg),
            _ if path.is_none() => path = Some(arg),
            _ => return Err(format!("unexpected argument `{arg}`")),
        }
    }
    let mut command = command.unwrap_or_else(|| "help".into());
    // `arcc file.as` is short for `arcc run file.as`.
    if is_program(&command) {
        if let Some(p) = path.take() {
            program_args.insert(0, p);
        }
        path = Some(command);
        command = "run".into();
    }
    Ok(Args { command, path, release, output, program_args })
}

fn is_program(word: &str) -> bool {
    project::is_source(word) || word.ends_with(".arc")
}

fn status(verb: &str, msg: &str) {
    println!("{GREEN}{verb:>12}{RESET} {msg}");
}

/// Diagnostics with their headings coloured.
fn report(text: &str) {
    let text = text.trim_end().replace("error:", "\x1b[91merror:\x1b[0m").replace("warning:", "\x1b[93mwarning:\x1b[0m");
    println!("{text}");
}

fn options(args: &Args) -> CompileOptions {
    if args.release {
        CompileOptions::RELEASE
    } else {
        CompileOptions::DEBUG
    }
}

fn path_or_cwd(args: &Args) -> String {
    args.path.clone().unwrap_or_else(fs::current_dir)
}

fn format_size(bytes: usize) -> String {
    if bytes >= 1 << 20 {
        format!("{:.2} MiB", bytes as f64 / (1u64 << 20) as f64)
    } else if bytes >= 1 << 10 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// Compiles, printing warnings; `Err` holds the exit code.
fn compile(input: &Input, opts: CompileOptions) -> Result<ArcPackage, i32> {
    match input.compile(opts) {
        Ok((pkg, warnings, source)) => {
            for w in &warnings {
                report(&w.render(&source));
            }
            Ok(pkg)
        }
        Err(fail) => {
            report(&fail.render());
            Err(1)
        }
    }
}

fn load_package(path: &str) -> Result<ArcPackage, i32> {
    let bytes = fs::read(path).map_err(|e| {
        eprintln!("{path}: {}", e.as_str());
        EXIT_BAD_PACKAGE
    })?;
    ArcPackage::from_bytes(&bytes).map_err(|e| {
        eprintln!("{path}: {e}");
        EXIT_BAD_PACKAGE
    })
}

/// A package from a `.arc`, or compiled from a source or project.
fn package_for(path: &str, opts: CompileOptions) -> Result<ArcPackage, i32> {
    if path.ends_with(".arc") {
        return load_package(path);
    }
    let input = Input::resolve(path).map_err(|e| {
        eprintln!("{e}");
        1
    })?;
    compile(&input, opts)
}

fn cmd_run(args: &Args) -> i32 {
    let path = path_or_cwd(args);
    let package = match package_for(&path, options(args)) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let name = package.manifest.get("name").unwrap_or("ArcScript").to_owned();
    let title = package.manifest.get("title").unwrap_or(&name).to_owned();
    let app_id = package.manifest.get("app_id").map_or_else(|| arcc_core::package::app_id(&name), str::to_owned);
    // A finished package runs as its app, in the app's sandbox; a source
    // being written runs as whoever started it.
    if path.ends_with(".arc") {
        if let Err(e) = justrt::app::enter(&app_id, &title) {
            eprintln!("{path}: cannot run as {app_id}: {}", e.as_str());
            return EXIT_BAD_PACKAGE;
        }
    }
    let mut vm = match Vm::with_host(package.program, Box::new(host::JustHost)) {
        Ok(vm) => vm,
        Err(e) => {
            eprintln!("{path}: {e}");
            return EXIT_BAD_PACKAGE;
        }
    };
    vm.set_args(args.program_args.clone());
    vm.set_app(&title, &app_id);
    let result = vm.run_main().and_then(|()| {
        if vm.has_view() {
            let nodes = vm.render_view()?;
            println!("{DIM}(the window of {title}, as text: open it in ArcStudio to use it){RESET}");
            print!("{}", UiNode::render_text(&nodes));
        }
        Ok(())
    });
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

fn cmd_build(args: &Args) -> i32 {
    let input = match Input::resolve(&path_or_cwd(args)) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let profile = if args.release { "release" } else { "debug" };
    status("Compiling", &format!("{} v{} ({}) [{profile}]", input.name, input.version, input.source_name));
    let started = justrt::time::uptime_ms();
    let pkg = match compile(&input, options(args)) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let output = args.output.clone().unwrap_or_else(|| {
        if input.is_project {
            // <project>/build/<profile>/<name>.arc, as everywhere else.
            let build = project::join(&input.root, "build");
            let _ = fs::make_dir(&build);
            let dir = project::join(&build, profile);
            let _ = fs::make_dir(&dir);
            project::join(&dir, &format!("{}.arc", input.name))
        } else {
            project::join(&input.root, &format!("{}.arc", input.name))
        }
    });
    let bytes = pkg.to_bytes(true);
    if let Err(e) = fs::write(&output, &bytes) {
        eprintln!("{output}: {}", e.as_str());
        return 1;
    }
    let ms = justrt::time::uptime_ms() - started;
    status("Finished", &format!("{output} ({}) in {}.{:02}s", format_size(bytes.len()), ms / 1000, ms % 1000 / 10));
    0
}

fn cmd_check(args: &Args) -> i32 {
    let input = match Input::resolve(&path_or_cwd(args)) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    match input.compile(options(args)) {
        Ok((_, warnings, source)) => {
            for w in &warnings {
                report(&w.render(&source));
            }
            status("Checked", &format!("{} - no errors, {} warning(s)", input.source_name, warnings.len()));
            0
        }
        Err(fail) => {
            report(&fail.render());
            1
        }
    }
}

fn cmd_disasm(args: &Args) -> i32 {
    match package_for(&path_or_cwd(args), options(args)) {
        Ok(pkg) => {
            print!("{}", pkg.program.disassemble());
            0
        }
        Err(code) => code,
    }
}

/// What `arc-runner --info` shows.
fn cmd_info(args: &Args) -> i32 {
    let Some(path) = &args.path else {
        eprintln!("usage: arcc info <file.arc>");
        return 2;
    };
    let pkg = match load_package(path) {
        Ok(p) => p,
        Err(code) => return code,
    };
    println!("{ACCENT}{path}{RESET}");
    for (k, v) in pkg.manifest.iter() {
        println!("  {k:<10} {v}");
    }
    let p = &pkg.program;
    println!("  {:<10} {} functions, {} constants, {} globals", "program", p.functions.len(), p.constants.len(), p.globals.len());
    println!("  {:<10} {}", "view", if p.view.is_some() { "yes" } else { "no" });
    for a in &pkg.assets {
        println!("  {:<10} {} ({})", "asset", a.name, format_size(a.data.len()));
    }
    0
}

fn cmd_new(args: &Args) -> i32 {
    let Some(dir) = &args.path else {
        eprintln!("usage: arcc new <folder>");
        return 2;
    };
    let name = project::file_name(dir);
    match project::scaffold(dir, name) {
        Ok(()) => {
            status("Created", &format!("project `{name}` in {dir}"));
            println!("             next: arcc run {dir}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn main(args: &str) -> i32 {
    let args = match parse_args(justrt::split_args(args)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            println!("{HELP}");
            return 2;
        }
    };
    match args.command.as_str() {
        "help" => {
            println!("{HELP}");
            0
        }
        "version" => {
            println!("arcc {} (JustOS)", arcc_core::VERSION);
            0
        }
        "run" => cmd_run(&args),
        "build" => cmd_build(&args),
        "check" => cmd_check(&args),
        "disasm" => cmd_disasm(&args),
        "info" => cmd_info(&args),
        "new" => cmd_new(&args),
        other => {
            eprintln!("unknown command `{other}` (see arcc help)");
            2
        }
    }
}
