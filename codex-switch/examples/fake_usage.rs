//! Offline quota transport fixture; no credentials, real Codex, or network.
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;

fn emit(value: Value) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{value}").unwrap();
    out.flush().unwrap();
}
fn main() {
    let home = PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
    let config: Value =
        serde_json::from_slice(&std::fs::read(home.join("fixture.json")).unwrap()).unwrap();
    let args: Vec<_> = std::env::args().skip(1).collect();
    let scenario = config["scenario"].as_str().unwrap_or("healthy");
    let catalog = args.iter().any(|a| a.starts_with("model_catalog_json="));
    if scenario == "stale-catalog" && catalog {
        eprintln!("failed to parse model_catalog_json: missing field new_runtime_field");
        return;
    }
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join(format!("transport-{}.jsonl", std::process::id())))
        .unwrap();
    let mut calls = 0;
    for line in std::io::stdin().lock().lines() {
        let v: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let method = v["method"].as_str().unwrap_or("");
        if method == "initialized" {
            continue;
        }
        let now = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        };
        writeln!(log, "{}", json!({"event":"start","time":now(),"method":method,"params":v["params"],"catalog":catalog})).unwrap();
        log.flush().unwrap();
        let result = match method {
            "initialize" => {
                if scenario == "slow-init" {
                    std::thread::sleep(std::time::Duration::from_millis(600));
                }
                assert!(args.iter().any(|s| s == "features.plugins=false"));
                assert!(args.iter().any(|s| s == "features.remote_plugin=false"));
                Ok(json!({"userAgent":"synthetic-usage"}))
            }
            "account/rateLimits/read" => {
                calls += 1;
                let lightweight = v["params"]["excludeResetCreditDetails"] == true;
                if scenario == "legacy" && lightweight {
                    Err("Invalid params: unknown field excludeResetCreditDetails")
                } else if scenario == "auth" {
                    Err("401 Unauthorized: authentication required")
                } else if scenario == "transient" && calls == 1 {
                    Err("error sending request: connection reset")
                } else if !lightweight && scenario != "legacy" {
                    Err("unnecessary detail endpoint stalled")
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(120));
                    Ok(json!({"rateLimitsByLimitId":{"codex":{
                        "primary":{"usedPercent":config["used"],"windowDurationMins":300,"resetsAt":1900000000},
                        "secondary":{"usedPercent":30,"windowDurationMins":10080,"resetsAt":1900600000}
                    }},"rateLimitResetCredits":{"availableCount":2}}))
                }
            }
            _ => Err("unexpected method"),
        };
        writeln!(
            log,
            "{}",
            json!({"event":"end","time":now(),"method":method})
        )
        .unwrap();
        log.flush().unwrap();
        match result {
            Ok(value) => emit(json!({"id":v["id"],"result":value})),
            Err(error) => emit(json!({"id":v["id"],"error":{"code":-32603,"message":error}})),
        }
    }
}
