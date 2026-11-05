//! What ArcScript programs reach on JustOS: the console, the clock, the
//! files of the mounted volumes and the other programs.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use arc_vm::{Host, RunOutput};
use justrt::{console, fs, process, time};

/// Seconds from 1970-01-01 to 2000-01-01, where the JustOS clock starts.
const UNIX_2000: u64 = 946_684_800;

pub struct JustHost;

fn fail(path: &str, e: justrt::Error) -> String {
    format!("{path}: {}", e.as_str())
}

/// Puts an argument in double quotes when it has spaces (see
/// `justrt::split_args`).
pub fn quote(arg: &str) -> String {
    if arg.is_empty() || arg.contains(char::is_whitespace) {
        format!("\"{arg}\"")
    } else {
        arg.to_string()
    }
}

impl Host for JustHost {
    fn print(&mut self, text: &str) {
        console::write(text);
        console::write("\n");
    }

    fn read_line(&mut self, prompt: Option<&str>) -> Option<String> {
        if let Some(p) = prompt {
            console::write(p);
        }
        console::read_line()
    }

    fn now(&mut self) -> f64 {
        (time::now() + UNIX_2000) as f64
    }

    fn clock_ms(&mut self) -> u64 {
        time::uptime_ms()
    }

    fn sleep_ms(&mut self, ms: u64) {
        time::sleep_ms(ms);
    }

    fn entropy(&mut self) -> u64 {
        let local = 0u8;
        (time::now() << 32) ^ time::uptime_ms() ^ (&local as *const u8 as u64)
    }

    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, String> {
        fs::read(path).map_err(|e| fail(path, e))
    }

    fn write_file(&mut self, path: &str, data: &[u8], append: bool) -> Result<(), String> {
        let mut f = if append { fs::File::append(path) } else { fs::File::create(path) }.map_err(|e| fail(path, e))?;
        f.write(data).map(drop).map_err(|e| fail(path, e))
    }

    fn file_kind(&mut self, path: &str) -> Option<bool> {
        if fs::list_dir(path).is_ok() {
            Some(true)
        } else {
            fs::File::open(path).ok().map(|_| false)
        }
    }

    fn list_dir(&mut self, path: &str) -> Result<Vec<String>, String> {
        fs::list_dir(path).map(|l| l.into_iter().map(|e| e.name).collect()).map_err(|e| fail(path, e))
    }

    fn make_dir(&mut self, path: &str) -> Result<(), String> {
        // Every folder on the way: "sys:/a/b" makes "sys:/a", then "sys:/a/b".
        let mut at = 0;
        loop {
            let end = path[at..].find('/').map(|i| at + i);
            let part = &path[..end.unwrap_or(path.len())];
            if !part.is_empty() && !part.ends_with(':') && !part.ends_with('/') {
                match fs::make_dir(part) {
                    Ok(()) | Err(justrt::Error::Exists) => {}
                    Err(e) => return Err(fail(part, e)),
                }
            }
            match end {
                Some(e) => at = e + 1,
                None => return Ok(()),
            }
        }
    }

    fn remove(&mut self, path: &str) -> Result<(), String> {
        fs::remove(path).map_err(|e| fail(path, e))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        fs::rename(from, to).map_err(|e| fail(from, e))
    }

    fn cwd(&mut self) -> String {
        fs::current_dir()
    }

    /// The program writes to the console itself: its output is not captured.
    fn run(&mut self, program: &str, args: &[String]) -> Result<RunOutput, String> {
        let args: Vec<String> = args.iter().map(|a| quote(a)).collect();
        let pid = process::spawn(program, &args.join(" ")).map_err(|e| fail(program, e))?;
        let code = process::wait(pid).map_err(|e| fail(program, e))?;
        Ok(RunOutput { code, ..RunOutput::default() })
    }

    fn os_name(&self) -> &'static str {
        "justos"
    }
}
