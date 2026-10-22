//! 浏览器登录新账号：借 `codex app-server` 自己的 OAuth 流程，把凭证写进隔离目录，
//! 再导入成账号。全程不碰当前活动的 Codex 登录。

use crate::codex;
use crate::rpc::AppServer;
use crate::store::{Account, Store};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub fn login_new_account(cancel: &AtomicBool) -> Result<Account> {
    let store = Store::discover()?;
    let executable = codex::locate().context("没有找到 codex.exe，无法使用浏览器登录")?;

    // 隔离目录：登录期间 codex 只写这里，原来的账号完全不受影响。
    let home = store.root.join(format!("login-{}", std::process::id()));
    if home.exists() {
        let _ = std::fs::remove_dir_all(&home);
    }
    std::fs::create_dir_all(&home)
        .with_context(|| format!("创建临时登录目录失败：{}", home.display()))?;

    let outcome = (|| -> Result<Account> {
        let mut server = AppServer::start(&executable, &home)?;
        server.initialize()?;

        let start = server.request(
            "account/login/start",
            serde_json::json!({
                "type": "chatgpt",
                "useHostedLoginSuccessPage": true,
                "appBrand": "codex"
            }),
        )?;
        let auth_url = start
            .get("authUrl")
            .and_then(Value::as_str)
            .context("登录接口没有返回 authUrl")?;
        open_url(auth_url)?;

        let completion = server.wait_notification_cancellable(
            "account/login/completed",
            Duration::from_secs(600),
            Some(cancel),
        )?;
        let params = completion.get("params").cloned().unwrap_or(Value::Null);
        let success = params
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !success {
            let message = params
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("浏览器登录未完成");
            bail!("浏览器登录失败：{message}");
        }

        let credential = home.join("auth.json");
        if !credential.exists() {
            bail!("登录完成但没有生成 auth.json");
        }
        store.import(&credential)
    })();

    let _ = std::fs::remove_dir_all(&home);
    outcome
}

fn open_url(url: &str) -> Result<()> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let operation = to_wide("open");
    let target = to_wide(url);
    unsafe {
        let result = ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        );
        if (result as isize) <= 32 {
            bail!("无法打开浏览器（ShellExecute 返回 {}）", result as isize);
        }
    }
    Ok(())
}

fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
