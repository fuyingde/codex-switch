//! Test harness for the actual quota reader and UI pool. Never packaged.
#![allow(dead_code)]
#[path = "../src/rpc.rs"]
mod rpc;
#[path = "../src/usage.rs"]
mod usage;
#[path = "../src/ui_usage.rs"]
mod workers;
mod win {
    pub const CREATE_NO_WINDOW: u32 = 0x08000000;
}
use std::path::PathBuf;
struct Request {
    id: String,
    home: PathBuf,
}
enum Message {
    Usage {
        id: String,
        result: Result<usage::Usage, String>,
    },
}
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let exe = PathBuf::from(&args[0]);
    let (tx, rx, _control) = workers::spawn(
        args[1].parse().unwrap(),
        move |request| usage::read(&exe, &request.home).map_err(|e| format!("{e:#}")),
        || {},
    );
    for (i, home) in args[2..].iter().enumerate() {
        tx.send(Request {
            id: i.to_string(),
            home: PathBuf::from(home),
        })
        .unwrap();
    }
    for _ in &args[2..] {
        let Message::Usage { id, result } =
            rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
        let output = match result {
            Ok(value) => {
                serde_json::json!({"id":id,"five":value.five_hour_remaining,"weekly":value.weekly_remaining,"resets":value.reset_credits})
            }
            Err(error) => serde_json::json!({"id":id,"error":error}),
        };
        println!("{output}");
    }
}
