//! Offline Desktop lifecycle fixture. Never distributed.
use serde_json::json;
use std::io::Write;
use std::process::{Command, Stdio};

fn main() {
    let home = std::path::PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
    let cli = std::env::var_os("CODEX_CLI_PATH")
        .unwrap_or_else(|| std::env::var_os("CODEX_SWITCH_REAL_CLI").unwrap());
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join("desktop-launches.jsonl"))
        .unwrap();
    writeln!(log, "{}", json!({"pid":std::process::id(),"cli":cli.to_string_lossy(),
        "account":serde_json::from_slice::<serde_json::Value>(&std::fs::read(home.join("auth.json")).unwrap()).unwrap()["tokens"]["account_id"]})).unwrap();
    let mut child = Command::new(cli)
        // Match the real Desktop's global arguments and lack of initialized notification.
        .args([
            "-c",
            "features.code_mode_host=true",
            "app-server",
            "--analytics-default-enabled",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "{}", json!({"id":42,"method":"initialize","params":{
        "clientInfo":{"name":"desktop_fixture","version":"1"},"capabilities":{"experimentalApi":false}}})).unwrap();
    input.flush().unwrap();
    if let Ok(delay) = std::env::var("CODEX_SWITCH_FIXTURE_TURN_MS") {
        writeln!(
            input,
            "{}",
            json!({"id":43,"method":"turn/start","params":{
            "threadId":"old-desktop-turn","durationMs":delay.parse::<u64>().unwrap()}})
        )
        .unwrap();
        input.flush().unwrap();
    }
    let _ = child.wait();
}
