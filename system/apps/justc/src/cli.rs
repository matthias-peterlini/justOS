//! The commands of the two JustScript programs of justOS, which share them:
//!
//! - `justc`, the compiler: builds `.jt` programs and projects into `.jpk`
//!   packages (or Windows `.exe` programs that carry one, `-t windows`),
//!   checks and disassembles them, and runs both (`justc run`);
//! - `just`, the runner: `just main.jt` or `just app.jpk` runs a program,
//!   `just --info app.jpk` describes a package. The shell and Files start
//!   `.jt` and `.jpk` files through it.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use just_vm::{UiNode, Vm};
use justc_core::jpk::Jpk;
use justc_core::CompileOptions;
use justrt::console::color::*;
use justrt::{eprintln, fs, print, println};
use crate::project::{self, Input};
use crate::host;

/// Which of the two programs is running.
#[derive(Clone, Copy)]
#[allow(dead_code, reason = "each of the two programs builds one of them")]
pub enum Tool {
    /// `justc`, the compiler.
    Compiler,
    /// `just`, the runner.
    Runner,
}

const HELP: &str = "\
justc - the JustScript compiler of justOS

  justc run <PATH> [ARGUMENTS...]   compile and run (also a .jpk package)
  justc <FILE.jt|FILE.jpk> [ARGS]   the same
  justc build <PATH> [-r] [-t TARGET] [-o OUT]
                                    compile to a .jpk package (or an .exe)
  justc check <PATH>                report errors and warnings only
  justc disasm <PATH>               the bytecode of a .jt or .jpk
  justc info <FILE.jpk|FILE.exe>    manifest and contents of a package
  justc new <FOLDER>                a new project

PATH is a .jt file or a project folder with a Just.toml (default: the
current folder). -r builds without debug information. TARGET is jpk (a
package, the default) or windows: an .exe with the runtime inside, which
runs on Windows by itself. Arguments after
PATH (or after --) are the program's: os.args().
Programs with a view are shown as text here; JustStudio, in the desktop,
shows them live. just <FILE> [ARGUMENTS...] runs a program too.";

const RUNNER_HELP: &str = "\
just - runs JustScript programs on justOS

  just <FILE.jt|FILE.jpk|FOLDER> [ARGUMENTS...]   run a program
  just --info <FILE.jpk>                          manifest and contents of a package

The arguments after the program are its own: os.args(). The shell and
Files start .jt and .jpk files through just; justc builds packages.";

/// Exit codes of JustScript's runtimes (BSD `EX_SOFTWARE`, `EX_DATAERR`).
const EXIT_RUNTIME_ERROR: i32 = 70;
const EXIT_BAD_PACKAGE: i32 = 65;

struct Args {
    command: String,
    path: Option<String>,
    release: bool,
    output: Option<String>,
    /// `-t`: what `build` makes.
    target: Option<String>,
    program_args: Vec<String>,
}

fn parse_args(words: Vec<String>) -> Result<Args, String> {
    let mut it = words.into_iter();
    let (mut command, mut path, mut release, mut output, mut target) = (None, None, false, None, None);
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
            "-t" | "--target" => target = Some(it.next().ok_or("missing value for --target")?),
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option `{s}`")),
            _ if command.is_none() => command = Some(arg),
            _ if path.is_none() => path = Some(arg),
            _ => return Err(format!("unexpected argument `{arg}`")),
        }
    }
    let mut command = command.unwrap_or_else(|| "help".into());
    // `justc file.jt` is short for `justc run file.jt`.
    if is_program(&command) {
        if let Some(p) = path.take() {
            program_args.insert(0, p);
        }
        path = Some(command);
        command = "run".into();
    }
    Ok(Args { command, path, release, output, target, program_args })
}

/// `just`'s arguments: the program and its own arguments, or an option.
fn runner_args(words: Vec<String>) -> Args {
    let mut it = words.into_iter();
    let first = it.next();
    let command = match first.as_deref() {
        // Nothing to run is a usage error.
        None => "usage",
        Some("-h" | "--help") => "help",
        Some("-V" | "--version") => "version",
        Some("--info") => "info",
        Some(_) => "run",
    };
    let path = match command {
        "run" => first,
        "info" => it.next(),
        _ => None,
    };
    Args { command: command.to_owned(), path, release: false, output: None, target: None, program_args: it.collect() }
}

fn is_program(word: &str) -> bool {
    project::is_source(word) || word.ends_with(".jpk")
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
fn compile(input: &Input, opts: CompileOptions) -> Result<Jpk, i32> {
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

fn load_package(path: &str) -> Result<Jpk, i32> {
    let bytes = fs::read(path).map_err(|e| {
        eprintln!("{path}: {}", e.as_str());
        EXIT_BAD_PACKAGE
    })?;
    // A program from `justc build -t windows` carries its package.
    let package = match jpk_bundler::format(&bytes) {
        Some(_) => jpk_bundler::payload(&bytes).map_err(|e| {
            eprintln!("{path}: {e}");
            EXIT_BAD_PACKAGE
        })?,
        None => &bytes[..],
    };
    Jpk::from_bytes(package).map_err(|e| {
        eprintln!("{path}: {e}");
        EXIT_BAD_PACKAGE
    })
}

/// What `justc build` makes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    /// A `.jpk` package (the default).
    Jpk,
    /// A Windows `.exe`: the runtime with the package appended.
    Windows,
}

impl Target {
    fn parse(name: Option<&str>) -> Result<Target, String> {
        match name {
            None | Some("jpk") => Ok(Target::Jpk),
            Some("windows" | "win" | "exe") => Ok(Target::Windows),
            Some(other) => Err(format!("unknown target `{other}`: jpk (a package, the default) or windows (an .exe)")),
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Target::Jpk => "jpk",
            Target::Windows => "exe",
        }
    }
}

/// The runtime `justc build -t windows` puts in front of the package.
const WINDOWS_RUNTIME: &str = "sys:/system/justc/just-stub.exe";

/// A package from a `.jpk`, or compiled from a source or project.
fn package_for(path: &str, opts: CompileOptions) -> Result<Jpk, i32> {
    if path.ends_with(".jpk") {
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
    let name = package.manifest.get("name").unwrap_or("JustScript").to_owned();
    let title = package.manifest.get("title").unwrap_or(&name).to_owned();
    let app_id = package.manifest.get("app_id").map_or_else(|| justc_core::jpk::app_id(&name), str::to_owned);
    // A finished package runs as its app, in the app's sandbox; a source
    // being written runs as whoever started it.
    if path.ends_with(".jpk") {
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
            println!("{DIM}(the window of {title}, as text: open it in JustStudio to use it){RESET}");
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
    let target = match Target::parse(args.target.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
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
    let mut pkg = match compile(&input, options(args)) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let ext = target.extension();
    let output = args.output.clone().unwrap_or_else(|| {
        if input.is_project {
            // <project>/build/<profile>/<name>.jpk, as everywhere else.
            let build = project::join(&input.root, "build");
            let _ = fs::make_dir(&build);
            let dir = project::join(&build, profile);
            let _ = fs::make_dir(&dir);
            project::join(&dir, &format!("{}.{ext}", input.name))
        } else {
            project::join(&input.root, &format!("{}.{ext}", input.name))
        }
    });
    let bytes = match target {
        Target::Jpk => pkg.to_bytes(true),
        Target::Windows => {
            pkg.manifest.set("target", "windows");
            let runtime = match fs::read(WINDOWS_RUNTIME) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("{WINDOWS_RUNTIME}: {} (the runtime of Windows programs)", e.as_str());
                    return 1;
                }
            };
            match jpk_bundler::bundle(&runtime, &pkg.to_bytes(true)) {
                Ok(exe) => exe,
                Err(e) => {
                    eprintln!("{WINDOWS_RUNTIME}: {e}");
                    return 1;
                }
            }
        }
    };
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

/// What `justc info` and `just --info` show.
fn cmd_info(args: &Args) -> i32 {
    let Some(path) = &args.path else {
        eprintln!("usage: justc info <file.jpk|file.exe> (or just --info <file.jpk>)");
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
        eprintln!("usage: justc new <folder>");
        return 2;
    };
    let name = project::file_name(dir);
    match project::scaffold(dir, name) {
        Ok(()) => {
            status("Created", &format!("project `{name}` in {dir}"));
            println!("             next: justc run {dir}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

pub fn main(args: &str, tool: Tool) -> i32 {
    let (name, help) = match tool {
        Tool::Compiler => ("justc", HELP),
        Tool::Runner => ("just", RUNNER_HELP),
    };
    let words = justrt::split_args(args);
    let args = match tool {
        Tool::Compiler => parse_args(words),
        Tool::Runner => Ok(runner_args(words)),
    };
    let args = match args {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            println!("{help}");
            return 2;
        }
    };
    match args.command.as_str() {
        "help" => {
            println!("{help}");
            0
        }
        "usage" => {
            println!("{help}");
            2
        }
        "version" => {
            println!("{name} {} (justOS)", justc_core::VERSION);
            0
        }
        "run" => cmd_run(&args),
        "build" => cmd_build(&args),
        "check" => cmd_check(&args),
        "disasm" => cmd_disasm(&args),
        "info" => cmd_info(&args),
        "new" => cmd_new(&args),
        other => {
            eprintln!("unknown command `{other}` (see {name} help)");
            2
        }
    }
}
