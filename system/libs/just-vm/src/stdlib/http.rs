//! `import http`: HTTP and HTTPS requests (blocking), sent by the host.
//!
//! Network failures are `Err(message)`; any HTTP response, including 404 or
//! 500, is `Ok({status, body, headers})`.

use super::text;
use crate::prelude::*;
use crate::value::Value;
use crate::vm::Vm;

pub(super) fn call(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, String> {
    let (method, url, body, headers) = match name {
        "http.get" => ("GET".to_owned(), &args[0], None, args.get(1)),
        "http.post" => ("POST".to_owned(), &args[0], args.get(1), args.get(2)),
        "http.request" => (text(&args[0], name)?.to_uppercase(), &args[1], args.get(2), args.get(3)),
        _ => return Err(format!("builtin `{name}` is not implemented by this runtime")),
    };
    let url = text(url, name)?;
    let mut header_list = Vec::new();
    match headers {
        None | Some(Value::Nil) => {}
        Some(Value::Map(m)) => {
            for (k, v) in m.borrow().iter() {
                header_list.push((k.to_value().to_string(), v.to_string()));
            }
        }
        Some(other) => return Err(format!("`{name}`: headers must be a map, found {}", other.type_name())),
    }
    let body = match body {
        None | Some(Value::Nil) => None,
        Some(Value::Str(s)) => Some(s.to_string()),
        // Maps, lists and structs are sent as JSON.
        Some(v @ (Value::Map(_) | Value::List(_) | Value::Struct(_))) => {
            if !header_list.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
                header_list.push(("Content-Type".into(), "application/json".into()));
            }
            Some(super::json::stringify(v, None)?)
        }
        Some(other) => Some(other.to_string()),
    };
    let request = crate::vm::HttpRequest { method: &method, url, body: body.as_deref(), headers: &header_list };
    Ok(match vm.host.http(&request) {
        Ok(response) => {
            let headers = response.headers.into_iter().map(|(k, v)| (crate::value::MapKey::Str(k.into()), Value::str(&v))).collect();
            vm.ok(super::record([
                ("status", Value::Int(response.status)),
                ("body", Value::str(&response.body)),
                ("headers", Value::map(headers)),
            ]))
        }
        Err(e) => vm.err(e),
    })
}
