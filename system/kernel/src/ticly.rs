//! The ticly account, kept on this computer: one sign-in for the ticly
//! apps (Do, AI, the browser). For now nothing goes over the network:
//! accounts live in `sys:/system/ticly/accounts.json` (name, e-mail, and the
//! password as a salted, many-times hashed HMAC-SHA-256), and
//! `sys:/system/ticly/session` names who is signed in. The data of the apps
//! stays in the user's files (Promemoria, Note, Calendario).
//!
//! ticly AI has no model to answer with yet: [`chat`] says so.

use crate::fs;
use crate::sync::IrqLock;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use justjson::Value;
use justtls::hash::{hmac, Alg};

const DIR: &str = "sys:/system/ticly";
/// Rounds of HMAC over the password.
const ROUNDS: usize = 4096;

fn read(path: &str) -> Option<Vec<u8>> {
    fs::abs(path).and_then(|p| fs::read_file(&p)).ok()
}

fn write(path: &str, data: &[u8]) -> Result<(), String> {
    if let Ok(d) = fs::abs(DIR) {
        if !fs::exists(&d) {
            fs::make_dir(&d).map_err(|e| e.to_string())?;
        }
    }
    fs::abs(path).and_then(|p| fs::write_file(&p, data)).map_err(|e| e.to_string())
}

/// Who is signed in.
#[derive(Clone, Default, Debug)]
pub struct Account {
    pub id: String,
    pub email: String,
    pub name: String,
    pub plan: String,
    /// When the account was made (seconds since 1970).
    pub created: u64,
}

impl Account {
    /// "Matthias" from "Matthias Peterlini" (or the e-mail's first part).
    pub fn first_name(&self) -> String {
        let n = self.name.split_whitespace().next().unwrap_or("");
        if !n.is_empty() {
            return n.to_string();
        }
        self.email.split('@').next().unwrap_or("").to_string()
    }

    /// "MP" for the avatar.
    pub fn initials(&self) -> String {
        let s: String = self.name.split_whitespace().take(2).filter_map(|w| w.chars().next()).collect();
        if s.is_empty() {
            self.email.chars().take(1).collect::<String>().to_uppercase()
        } else {
            s.to_uppercase()
        }
    }
}

/// An account as stored: the account and its salt and password hash.
struct Stored {
    account: Account,
    salt: String,
    hash: String,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn password_hash(salt: &str, password: &str) -> String {
    let mut h = hmac(Alg::Sha256, salt.as_bytes(), password.as_bytes());
    for _ in 1..ROUNDS {
        h = hmac(Alg::Sha256, salt.as_bytes(), &h);
    }
    hex(&h)
}

fn load() -> Vec<Stored> {
    let Some(v) = read(&format!("{DIR}/accounts.json")).and_then(|b| justjson::parse_bytes(&b)) else { return Vec::new() };
    v.get("accounts")
        .as_array()
        .iter()
        .map(|a| Stored {
            account: Account {
                id: a.get("id").str().to_string(),
                email: a.get("email").str().to_string(),
                name: a.get("name").str().to_string(),
                plan: String::from("FREE"),
                created: a.get("created").as_f64().unwrap_or(0.0) as u64,
            },
            salt: a.get("salt").str().to_string(),
            hash: a.get("hash").str().to_string(),
        })
        .collect()
}

fn save(list: &[Stored]) -> Result<(), String> {
    let arr: Vec<Value> = list
        .iter()
        .map(|s| {
            Value::object(alloc::vec![
                ("id", Value::string(&s.account.id)),
                ("email", Value::string(&s.account.email)),
                ("name", Value::string(&s.account.name)),
                ("created", Value::Num(s.account.created as f64)),
                ("salt", Value::string(&s.salt)),
                ("hash", Value::string(&s.hash)),
            ])
        })
        .collect();
    write(&format!("{DIR}/accounts.json"), Value::object(alloc::vec![("accounts", Value::Arr(arr))]).to_string().as_bytes())
}

static SESSION: IrqLock<Option<Option<Account>>> = IrqLock::new(None);

/// The signed-in account, if any.
pub fn account() -> Option<Account> {
    if let Some(a) = SESSION.lock().clone() {
        return a;
    }
    let email = read(&format!("{DIR}/session")).map(|b| String::from_utf8_lossy(&b).trim().to_string()).unwrap_or_default();
    let a = if email.is_empty() { None } else { load().into_iter().find(|s| s.account.email == email).map(|s| s.account) };
    *SESSION.lock() = Some(a.clone());
    a
}

fn start_session(a: &Account) -> Result<(), String> {
    write(&format!("{DIR}/session"), a.email.as_bytes())?;
    *SESSION.lock() = Some(Some(a.clone()));
    crate::info!("ticly", "signed in as {}", a.email);
    Ok(())
}

pub fn sign_out() {
    if let Ok(p) = fs::abs(&format!("{DIR}/session")) {
        let _ = fs::remove(&p);
    }
    *SESSION.lock() = Some(None);
}

fn valid_email(e: &str) -> bool {
    let Some((user, host)) = e.split_once('@') else { return false };
    !user.is_empty() && host.contains('.') && !host.starts_with('.') && !host.ends_with('.') && !e.contains(char::is_whitespace)
}

/// Signs in with an account of this computer.
pub fn sign_in(email: &str, password: &str) -> Result<Account, String> {
    let email = email.trim().to_lowercase();
    let list = load();
    let Some(s) = list.iter().find(|s| s.account.email == email) else {
        return Err(String::from("Non c'è un account con questa e-mail su questo computer: creane uno."));
    };
    if password_hash(&s.salt, password) != s.hash {
        return Err(String::from("Password errata."));
    }
    start_session(&s.account)?;
    Ok(s.account.clone())
}

/// Makes an account on this computer and signs in with it.
pub fn sign_up(name: &str, email: &str, password: &str) -> Result<Account, String> {
    let email = email.trim().to_lowercase();
    let name = name.trim();
    if name.is_empty() {
        return Err(String::from("Scrivi il tuo nome."));
    }
    if !valid_email(&email) {
        return Err(String::from("Questa e-mail non sembra valida."));
    }
    if password.chars().count() < 6 {
        return Err(String::from("La password deve avere almeno 6 caratteri."));
    }
    let mut list = load();
    if list.iter().any(|s| s.account.email == email) {
        return Err(String::from("Esiste già un account con questa e-mail: accedi."));
    }
    let salt = format!("{:016x}{:016x}", crate::rand::u64(), crate::rand::u64());
    let account = Account {
        id: format!("{:016x}", crate::rand::u64()),
        email,
        name: name.to_string(),
        plan: String::from("FREE"),
        created: crate::time::unix_now(),
    };
    let hash = password_hash(&salt, password);
    list.push(Stored { account: account.clone(), salt, hash });
    save(&list)?;
    start_session(&account)?;
    Ok(account)
}

/// A model of the catalog.
#[derive(Clone, Debug)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub min_plan: String,
    pub unlocked: bool,
}

/// The models ticly AI will offer.
pub fn default_models() -> Vec<Model> {
    [("openrouter/free", "Free Auto"), ("google/gemma-4-31b-it:free", "Gemma 4 31B"), ("nvidia/nemotron-3-super-120b-a12b:free", "Nemotron 3 Super")]
        .iter()
        .map(|(id, name)| Model { id: id.to_string(), name: name.to_string(), min_plan: String::from("FREE"), unlocked: true })
        .collect()
}

/// Why ticly AI does not answer.
pub const AI_UNAVAILABLE: &str = "ticly AI non è ancora disponibile su JustOS: le risposte arriveranno con il collegamento al servizio ticly. La conversazione resta salvata.";

/// Asks ticly AI (not connected yet: always the note above).
pub fn chat(_model: &str, _messages: &[(String, String)]) -> Result<String, String> {
    Err(String::from(AI_UNAVAILABLE))
}
