//! Offline transport fixture used by tests/test_seamless.py. Never installed or packaged.
use base64::Engine;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

fn emit(v: Value) {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "{v}").unwrap();
    stdout.flush().unwrap();
}

fn main() {
    if std::env::args().any(|a| a == "--version") {
        println!("fake-codex 1.0");
        return;
    }
    let home = std::path::PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
    let auth: Value =
        serde_json::from_slice(&std::fs::read(home.join("auth.json")).unwrap()).unwrap();
    let token = auth.pointer("/tokens/access_token").unwrap().clone();
    let account = auth.pointer("/tokens/account_id").unwrap().clone();
    let state = Arc::new(Mutex::new((token, account, "chatgpt".to_string())));
    let mut refresh_request = None;
    let mut corrupt_read = false;
    for line in std::io::stdin().lock().lines() {
        let v: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = v["id"].clone();
        if v["id"] == "refresh-fixture" && v.get("method").is_none() {
            if let Some(result) = v.get("result") {
                let mut s = state.lock().unwrap();
                s.0 = result["accessToken"].clone();
                emit(json!({"id":refresh_request.take(),"result":{"refreshed":true}}));
            } else {
                emit(
                    json!({"id":refresh_request.take(),"error":{"code":-1,"message":"refresh failed"}}),
                );
            }
            continue;
        }
        match v["method"].as_str().unwrap_or("") {
            "initialize" => emit(json!({"id":id,"result":{"userAgent":"fixture"}})),
            "initialized" => (),
            "getAuthStatus" => {
                if v.pointer("/params/refreshToken").and_then(Value::as_bool) == Some(true) {
                    let mut auth = auth.clone();
                    let mut claims: Value = serde_json::from_slice(
                        &base64::engine::general_purpose::URL_SAFE_NO_PAD
                            .decode(
                                state
                                    .lock()
                                    .unwrap()
                                    .0
                                    .as_str()
                                    .unwrap()
                                    .split('.')
                                    .nth(1)
                                    .unwrap(),
                            )
                            .unwrap(),
                    )
                    .unwrap();
                    claims["exp"] = json!(4102444800i64);
                    claims["jti"] = json!("rotated-by-fixture");
                    let token = format!(
                        "e30.{}.sig",
                        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string())
                    );
                    auth["tokens"]["access_token"] = json!(token);
                    std::fs::write(home.join("auth.json"), auth.to_string()).unwrap();
                    state.lock().unwrap().0 = json!(token);
                }
                let s = state.lock().unwrap();
                let token = if corrupt_read {
                    corrupt_read = false;
                    json!("wrong-token")
                } else {
                    s.0.clone()
                };
                emit(json!({"id":id,"result":{"authMethod":s.2,"authToken":token}}));
            }
            "account/login/start" => {
                let target = v["params"]["chatgptAccountId"].clone();
                if target == "reject" {
                    emit(json!({"id":id,"error":{"code":-1,"message":"rejected"}}));
                    continue;
                }
                corrupt_read = target == "bad-read";
                let mut s = state.lock().unwrap();
                s.0 = v["params"]["accessToken"].clone();
                s.1 = target;
                s.2 = "chatgptAuthTokens".into();
                emit(json!({"id":id,"result":{"type":"chatgptAuthTokens"}}));
                emit(
                    json!({"method":"account/updated","params":{"authMode":"chatgptAuthTokens","planType":"plus"}}),
                );
            }
            "turn/start" => {
                let thread = v["params"]["threadId"].clone();
                let s = state.lock().unwrap();
                let mut log = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(home.join("requests.jsonl"))
                    .unwrap();
                writeln!(
                    log,
                    "{}",
                    json!({"account":s.1,"thread":thread,"pid":std::process::id()})
                )
                .unwrap();
                emit(json!({"id":id,"result":{"turn":{"id":"turn-fixture"}}}));
                emit(
                    json!({"method":"turn/started","params":{"threadId":thread,"turn":{"id":"turn-fixture"}}}),
                );
                let delay = v
                    .pointer("/params/durationMs")
                    .and_then(Value::as_u64)
                    .unwrap_or(10);
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(delay));
                    emit(
                        json!({"method":"turn/completed","params":{"threadId":thread,"turn":{"id":"turn-fixture","status":"completed"}}}),
                    );
                });
            }
            "test/refresh" => {
                refresh_request = Some(id);
                let s = state.lock().unwrap();
                emit(
                    json!({"method":"account/chatgptAuthTokens/refresh","id":"refresh-fixture","params":{"reason":"unauthorized","previousAccountId":s.1}}),
                );
            }
            "test/ping" => emit(json!({"id":id,"result":{"pong":true}})),
            _ => emit(json!({"id":id,"error":{"code":-32601,"message":"unknown fixture method"}})),
        }
    }
}
