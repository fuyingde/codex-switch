//! 读取某个 profile 的额度：启动该 profile 的 app-server，调用 `account/rateLimits/read`。
//!
//! 解析规则与原作者一致：
//! - 窗口取 `rateLimitsByLimitId.codex`，退化到 `rateLimits`
//! - 5 小时窗口 = `windowDurationMins == 300`
//! - 每周窗口 = 6～8 天里最长的那个
//! - 剩余百分比 = round(100 - usedPercent)，夹在 0～100

use crate::rpc::AppServer;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub five_hour_remaining: Option<u8>,
    pub five_hour_resets_at: Option<i64>,
    pub weekly_remaining: Option<u8>,
    pub weekly_resets_at: Option<i64>,
    pub reset_credits: Option<u32>,
}

// Stagger child initialization, not quota requests. Simultaneously cold-starting
// many full runtimes needlessly competes for disk/CPU and startup network work.
static STARTUP: Mutex<()> = Mutex::new(());

pub fn read(codex: &Path, profile_home: &Path) -> Result<Usage> {
    let mut server = start_reader(codex, profile_home)?;
    let params = serde_json::json!({"excludeResetCreditDetails": true});
    let result = match server.request("account/rateLimits/read", params.clone()) {
        Ok(result) => result,
        Err(error) if unsupported_params(&error.to_string()) => {
            // Older runtimes may strictly reject the newer optional parameter.
            server.request("account/rateLimits/read", serde_json::json!({}))?
        }
        Err(error) if transient_network_error(&error.to_string()) => {
            // Retry one failed read, reusing the initialized child and its HTTP client.
            // Never restart Desktop, refresh another profile, or retry an auth rejection.
            std::thread::sleep(Duration::from_millis(250));
            server.request("account/rateLimits/read", params)?
        }
        Err(error) => return Err(error),
    };
    parse(&result)
}

fn query_overrides(home: &Path) -> Vec<String> {
    let mut overrides = vec![
        "features.plugins=false".into(),
        "features.remote_plugin=false".into(),
    ];
    // ModelsResponse ignores the cache metadata and reads its `models` array.
    // A quota-only child does not need to download a model catalog on every click.
    let cache = home.join("models_cache.json");
    let usable = std::fs::metadata(&cache)
        .ok()
        .filter(|m| m.len() <= 8 * 1024 * 1024)
        .and_then(|_| std::fs::read(&cache).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| {
            value.get("models").and_then(Value::as_array).map(|models| {
                !models.is_empty()
                    && models
                        .iter()
                        .all(|m| m.get("slug").and_then(Value::as_str).is_some())
            })
        })
        .unwrap_or(false);
    if usable && cache.is_absolute() {
        // JSON quoted strings are also valid TOML basic strings, including backslashes.
        overrides.push(format!(
            "model_catalog_json={}",
            serde_json::to_string(&cache.to_string_lossy()).unwrap()
        ));
    }
    overrides
}

fn start_reader(codex: &Path, home: &Path) -> Result<AppServer> {
    let mut overrides = query_overrides(home);
    let mut server = spawn_reader(codex, home, &overrides)?;
    if let Err(error) = server.initialize() {
        let text = error.to_string().to_lowercase();
        if overrides
            .iter()
            .any(|s| s.starts_with("model_catalog_json="))
            && [
                "model_catalog",
                "failed to parse",
                "missing field",
                "unknown field",
                "unknown variant",
                "app-server 提前退出",
                "写入 app-server 失败",
                "刷新 app-server stdin 失败",
            ]
            .iter()
            .any(|needle| text.contains(needle))
        {
            // A stale catalog schema must not make a saved account unusable.
            drop(server);
            overrides.retain(|s| !s.starts_with("model_catalog_json="));
            server = spawn_reader(codex, home, &overrides)?;
            server.initialize()?;
        } else {
            return Err(error);
        }
    }
    Ok(server)
}

fn spawn_reader(codex: &Path, home: &Path, overrides: &[String]) -> Result<AppServer> {
    let _startup = STARTUP
        .lock()
        .map_err(|_| anyhow::anyhow!("刷新服务初始化失败"))?;
    let server = AppServer::start_with_overrides(codex, home, overrides)?;
    // A small launch gap avoids a cold-start burst. Release before initialize()
    // so one stalled profile cannot hold every other account behind a global lock.
    std::thread::sleep(Duration::from_millis(50));
    Ok(server)
}

fn unsupported_params(text: &str) -> bool {
    let text = text.to_lowercase();
    text.contains("unknown field")
        || text.contains("invalid params")
        || text.contains("invalid parameters")
}

fn transient_network_error(text: &str) -> bool {
    let text = text.to_lowercase();
    // RPC timeouts have no result and may still be running: do not pile another
    // request onto that child. Permanent authentication errors also fail immediately.
    if [
        "401",
        "403",
        "unauthorized",
        "invalid_grant",
        "refresh token",
        "authentication",
    ]
    .iter()
    .any(|needle| text.contains(needle))
    {
        return false;
    }
    [
        "error sending request",
        "connection reset",
        "connection closed",
        "connection refused",
        "temporarily unavailable",
        "502 bad gateway",
        "503 service unavailable",
        "504 gateway timeout",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn parse(result: &Value) -> Result<Usage> {
    let bucket = result
        .get("rateLimitsByLimitId")
        .and_then(|value| value.get("codex"))
        .or_else(|| result.get("rateLimits"))
        .context("响应里没有速率限制数据（账号可能未登录或额度不可用）")?;

    let windows: Vec<Window> = ["primary", "secondary"]
        .iter()
        .filter_map(|key| bucket.get(*key).and_then(parse_window))
        .collect();

    let weekly = windows
        .iter()
        .filter(|window| (6 * 24 * 60..=8 * 24 * 60).contains(&window.minutes))
        .max_by_key(|window| window.minutes);

    if weekly.is_none() {
        bail!("响应里没有每周额度窗口");
    }

    let five_hour = windows.iter().find(|window| window.minutes == 5 * 60);
    let reset_credits = result
        .get("rateLimitResetCredits")
        .and_then(|value| value.get("availableCount"))
        .and_then(Value::as_u64)
        .map(|value| value as u32);

    Ok(Usage {
        five_hour_remaining: five_hour.map(|window| remaining(window.used_percent)),
        five_hour_resets_at: five_hour.map(|window| window.resets_at),
        weekly_remaining: weekly.map(|window| remaining(window.used_percent)),
        weekly_resets_at: weekly.map(|window| window.resets_at),
        reset_credits,
    })
}

struct Window {
    used_percent: f64,
    minutes: i64,
    resets_at: i64,
}

fn parse_window(value: &Value) -> Option<Window> {
    let used_percent = value.get("usedPercent").and_then(Value::as_f64)?;
    let minutes = value
        .get("windowDurationMins")
        .and_then(Value::as_i64)
        .or_else(|| value.get("durationMinutes").and_then(Value::as_i64))?;
    let resets_at = value.get("resetsAt").and_then(Value::as_f64)? as i64;
    Some(Window {
        used_percent,
        minutes,
        resets_at,
    })
}

fn remaining(used_percent: f64) -> u8 {
    (100.0 - used_percent).round().clamp(0.0, 100.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_only_transient_network_failures_not_auth_or_unfinished_rpc() {
        for text in [
            "error sending request: connection reset",
            "503 Service Unavailable",
        ] {
            assert!(transient_network_error(text));
        }
        for text in [
            "401 Unauthorized: error sending request",
            "403 Forbidden",
            "authentication required",
            "refresh token expired",
            "invalid_grant",
            "等待 app-server 响应超时",
            "request timed out",
            "响应里没有每周额度窗口",
        ] {
            assert!(!transient_network_error(text), "{text}");
        }
        assert!(unsupported_params(
            "Invalid params: unknown field excludeResetCreditDetails"
        ));
        assert!(!unsupported_params("401 Unauthorized"));
    }

    #[test]
    fn query_only_overrides_use_nonempty_model_cache_and_never_write_config_or_auth() {
        let home = std::env::temp_dir().join(format!(
            "codex-usage-options-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&home).unwrap();
        let cache = home.join("models_cache.json");
        let config = home.join("config.toml");
        let auth = home.join("auth.json");
        std::fs::write(&config, "# synthetic sentinel").unwrap();
        std::fs::write(&auth, "synthetic-no-credential").unwrap();
        assert_eq!(query_overrides(&home).len(), 2);
        for bytes in [r#"{"models":[]}"#, r#"{"models":[{}]}"#, "bad json"] {
            std::fs::write(&cache, bytes).unwrap();
            assert_eq!(query_overrides(&home).len(), 2);
        }
        let bytes = r#"{"models":[{"slug":"synthetic"}],"identity":"synthetic"}"#;
        std::fs::write(&cache, bytes).unwrap();
        let options = query_overrides(&home);
        assert_eq!(options.len(), 3);
        let encoded = options[2].strip_prefix("model_catalog_json=").unwrap();
        let decoded: String = serde_json::from_str(encoded).unwrap();
        assert_eq!(Path::new(&decoded), cache);
        assert_eq!(std::fs::read_to_string(&cache).unwrap(), bytes);
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "# synthetic sentinel"
        );
        assert_eq!(
            std::fs::read_to_string(&auth).unwrap(),
            "synthetic-no-credential"
        );
        for file in [cache, config, auth] {
            std::fs::remove_file(file).unwrap();
        }
        std::fs::remove_dir(home).unwrap();
    }
}
