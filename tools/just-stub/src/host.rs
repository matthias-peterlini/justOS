//! What a standalone JustScript program reaches on the system it runs on:
//! the console, the clock, the files and the other programs.

use just_vm::{Host, RunOutput};
use std::io::Write;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct StdHost {
    start: Instant,
}

impl StdHost {
    pub fn new() -> StdHost {
        StdHost { start: Instant::now() }
    }
}

fn fail(path: &str, e: std::io::Error) -> String {
    format!("{path}: {e}")
}

impl Host for StdHost {
    fn print(&mut self, text: &str) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{text}");
    }

    fn read_line(&mut self, prompt: Option<&str>) -> Option<String> {
        if let Some(p) = prompt {
            print!("{p}");
        }
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_owned()),
        }
    }

    fn now(&mut self) -> f64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
    }

    fn clock_ms(&mut self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn sleep_ms(&mut self, ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }

    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, String> {
        std::fs::read(path).map_err(|e| fail(path, e))
    }

    fn write_file(&mut self, path: &str, data: &[u8], append: bool) -> Result<(), String> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(path)
            .map_err(|e| fail(path, e))?;
        f.write_all(data).map_err(|e| fail(path, e))
    }

    fn file_kind(&mut self, path: &str) -> Option<bool> {
        std::fs::metadata(path).ok().map(|m| m.is_dir())
    }

    fn list_dir(&mut self, path: &str) -> Result<Vec<String>, String> {
        let entries = std::fs::read_dir(path).map_err(|e| fail(path, e))?;
        Ok(entries.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
    }

    fn make_dir(&mut self, path: &str) -> Result<(), String> {
        std::fs::create_dir_all(path).map_err(|e| fail(path, e))
    }

    fn remove(&mut self, path: &str) -> Result<(), String> {
        let p = std::path::Path::new(path);
        if p.is_dir() { std::fs::remove_dir(p) } else { std::fs::remove_file(p) }.map_err(|e| fail(path, e))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        std::fs::rename(from, to).map_err(|e| fail(from, e))
    }

    fn copy_file(&mut self, from: &str, to: &str) -> Result<(), String> {
        std::fs::copy(from, to).map(drop).map_err(|e| fail(from, e))
    }

    fn env(&mut self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn cwd(&mut self) -> String {
        std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Its exit code and its output, captured.
    fn run(&mut self, program: &str, args: &[String]) -> Result<RunOutput, String> {
        let out = std::process::Command::new(program).args(args).output().map_err(|e| fail(program, e))?;
        Ok(RunOutput {
            code: out.status.code().map_or(-1, i64::from),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn os_name(&self) -> &'static str {
        std::env::consts::OS
    }
}
