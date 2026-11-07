use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;

fn main() {
    let (tx, rx) = mpsc::channel();
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let tx = tx.clone();
            thread::spawn(move || {
                let s: u64 = (0..1_000_000u64).map(|x| x % (i + 2)).sum();
                tx.send(s).unwrap();
            })
        })
        .collect();
    drop(tx);
    let total: u64 = rx.iter().sum();
    for h in handles {
        h.join().unwrap();
    }
    println!("rt: threads total={total}");
    let mut m = HashMap::new();
    m.insert("just", 1);
    println!("rt: hashmap {}", m["just"]);
    std::fs::write("rt.txt", b"rust file").unwrap();
    println!("rt: file {}", std::fs::read_to_string("rt.txt").unwrap());
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let mut s = std::net::TcpStream::connect(&args[1]).unwrap();
        s.write_all(b"GET /hello.txt HTTP/1.0\r\n\r\n").unwrap();
        let mut r = String::new();
        s.read_to_string(&mut r).unwrap();
        println!("rt: http {}", r.split("\r\n\r\n").nth(1).unwrap_or("").trim());
    }
    let t0 = std::time::Instant::now();
    thread::sleep(std::time::Duration::from_millis(120));
    println!("rt: slept {}", t0.elapsed().as_millis() >= 100);
}
