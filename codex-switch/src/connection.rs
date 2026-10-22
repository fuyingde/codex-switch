//! Automatic connection setup. Reconnect the current account before any switch is attempted.
use crate::{bridge, desktop::Desktop, store::Store, win};
use anyhow::{bail, Context, Result};
use std::time::{Duration, Instant};

pub fn ensure(store: &Store, progress: impl Fn(&str)) -> Result<()> {
    let _guard = win::connection_lock(&store.root)?;
    let connected = || -> Result<bool> {
        let mut ready = 0;
        for session in bridge::registered_sessions(store)? {
            if session
                .request("status", None)
                .ok()
                .is_some_and(|v| compatible(&v))
            {
                ready += 1;
            }
        }
        if ready > 1 {
            bail!("同一目录有多个 Codex 后台，请关闭多余的后台后重试；账号未改动");
        }
        Ok(ready == 1)
    };
    if connected()? {
        // Refresh the next-launch entry, but keep the compatible running backend.
        bridge::install(store)?;
        return Ok(());
    }

    // Startup can take time and status can be delayed by authentication work.
    // Retry before deciding that a real connection is missing.
    if !bridge::registered_sessions(store)?.is_empty() {
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(350));
            if connected()? {
                bridge::install(store)?;
                return Ok(());
            }
        }
        let mut known_unready = false;
        let mut unresponsive = false;
        for session in bridge::registered_sessions(store)? {
            match session.request("status", None) {
                Ok(v) if v["ready"] == false => known_unready = true,
                Ok(v) if compatible(&v) => return Ok(()),
                _ => {
                    unresponsive = true;
                }
            }
        }
        if (unresponsive || !known_unready) && !bridge::registered_sessions(store)?.is_empty() {
            bail!("无感后台暂未响应，已保留当前 Codex；请稍后重试，不会因查询超时重启客户端");
        }
    }
    let wrapper = bridge::install(store)?;

    // An older bridge can still report active turns even if its initialization flag is wrong.
    // Let those turns finish before upgrading/reconnecting the Desktop process.
    let mut waiting = false;
    loop {
        let busy = bridge::live_sessions(store)?.iter().any(|s| {
            s.request("status", None)
                .ok()
                .is_some_and(|v| v["activeTurns"].as_u64().unwrap_or(0) > 0)
        });
        if !busy {
            break;
        }
        if !waiting {
            progress("等待当前轮结束，随后自动重新连接 Codex；账号未改动…");
            waiting = true;
        }
        std::thread::sleep(Duration::from_millis(500));
        if connected()? {
            return Ok(());
        }
    }

    let desktop = Desktop::discover().context("无法自动连接 Codex")?;
    // Desktop may have just launched and not yet published its bridge descriptor.
    if !desktop.running().is_empty() {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if connected()? {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    progress("正在自动连接，将重新打开 Codex；账号未改动…");
    if !desktop.running().is_empty() {
        desktop.close(true)?;
    }
    desktop.reopen_connected(&wrapper, &store.codex_home)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if connected()? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    bail!("Codex 已重新打开，但无感连接没有就绪。账号未改动；可从托盘选择“重新连接 Codex”重试");
}

fn compatible(status: &serde_json::Value) -> bool {
    status["ready"] == true
        && status
            .get("protocol")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1)
            == 1
}

/// Background health checks never install, start, close, or restart Desktop.
pub fn check_ready(store: &Store) -> Result<bool> {
    let mut count = 0;
    for session in bridge::registered_sessions(store)? {
        if session
            .request("status", None)
            .ok()
            .is_some_and(|v| compatible(&v))
        {
            count += 1;
        }
    }
    Ok(count == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_compatibility_does_not_depend_on_ui_build_hash() {
        assert!(compatible(
            &serde_json::json!({"ready":true,"build":"old-ui","protocol":1})
        ));
        assert!(compatible(
            &serde_json::json!({"ready":true,"version":"0.4.0"})
        ));
        assert!(!compatible(&serde_json::json!({"ready":true,"protocol":2})));
        assert!(!compatible(
            &serde_json::json!({"ready":false,"protocol":1})
        ));
    }
}
