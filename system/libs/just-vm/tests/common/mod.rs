//! A [`Host`] on the test machine's own system, so the tests check the
//! standard library as JustScript's `std` runtime has it.

use just_vm::{Host, RunOutput};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct StdHost {
    pub lines: Rc<RefCell<Vec<String>>>,
    start: Instant,
}

impl StdHost {
    pub fn new() -> (StdHost, Rc<RefCell<Vec<String>>>) {
        let lines = Rc::new(RefCell::new(Vec::new()));
        (StdHost { lines: Rc::clone(&lines), start: Instant::now() }, lines)
    }
}

fn text(e: std::io::Error) -> String {
    e.to_string()
}

impl Host for StdHost {
    fn print(&mut self, text: &str) {
        self.lines.borrow_mut().push(text.to_owned());
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
        std::fs::read(path).map_err(|e| format!("{path}: {e}"))
    }

    fn write_file(&mut self, path: &str, data: &[u8], append: bool) -> Result<(), String> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(path).map_err(text)?;
        f.write_all(data).map_err(text)
    }

    fn file_kind(&mut self, path: &str) -> Option<bool> {
        std::fs::metadata(path).ok().map(|m| m.is_dir())
    }

    fn list_dir(&mut self, path: &str) -> Result<Vec<String>, String> {
        let entries = std::fs::read_dir(path).map_err(|e| format!("{path}: {e}"))?;
        Ok(entries.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
    }

    fn make_dir(&mut self, path: &str) -> Result<(), String> {
        std::fs::create_dir_all(path).map_err(text)
    }

    fn remove(&mut self, path: &str) -> Result<(), String> {
        if std::path::Path::new(path).is_dir() { std::fs::remove_dir(path) } else { std::fs::remove_file(path) }.map_err(text)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        std::fs::rename(from, to).map_err(text)
    }

    fn env(&mut self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn cwd(&mut self) -> String {
        std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()
    }

    fn run(&mut self, program: &str, args: &[String]) -> Result<RunOutput, String> {
        let out = std::process::Command::new(program).args(args).output().map_err(|e| format!("{program}: {e}"))?;
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
