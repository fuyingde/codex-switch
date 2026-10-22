//! codex-switch：个人版 Codex 账号切换器。
//!
//! 不带参数启动就是图形界面；带参数时是命令行工具（env / id / list / usage / register / import / switch …）。

// 发布版不弹控制台窗口（双击即用）；debug 版保留控制台，方便看命令行输出。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod codex;
mod connection;
mod desktop;
mod identity;
mod login;
mod rpc;
mod store;
mod switch;
mod ui;
mod update;
mod usage;
mod win;

use anyhow::{bail, Result};
use std::path::Path;
use store::Store;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "--apply-update") {
        let result = args
            .get(1)
            .map(std::path::PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("缺少更新计划"))
            .and_then(|p| update::apply(&p));
        if let Err(error) = result {
            let text: Vec<u16> =
                format!("更新失败：{error:#}\n请重新打开原软件，或从官网下载新版。")
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
            let title: Vec<u16> = "Codex切号器".encode_utf16().chain(Some(0)).collect();
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                    std::ptr::null_mut(),
                    text.as_ptr(),
                    title.as_ptr(),
                    0x10,
                );
            }
        }
        return;
    }
    if args.first().is_some_and(|s| s == "--updated-from") {
        if let Some(path) = args.get(1).map(std::path::PathBuf::from) {
            std::thread::spawn(move || update::cleanup(path));
        }
        if let Err(error) = ui::run() {
            eprintln!("{error:#}");
        }
        return;
    }
    let proxy = std::env::current_exe()
        .ok()
        .is_some_and(|p| codex::is_bridge(&p));
    if proxy || args.first().is_some_and(|a| a == "bridge-run") {
        let args = if proxy {
            args
        } else {
            args.into_iter().skip(1).collect()
        };
        match bridge::run_proxy(args) {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                eprintln!("无感后台：{error:#}");
                std::process::exit(1);
            }
        }
    }
    let mut arguments = std::env::args().skip(1);
    let first = arguments.next();

    // A release GUI executable still needs a parent console for explicit CLI commands.
    // Preserve inherited pipes when Desktop launches the stdio proxy.
    if first.is_some() {
        unsafe {
            use windows_sys::Win32::System::Console::{
                AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
            };
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            if handle.is_null() || handle as isize == -1 {
                AttachConsole(ATTACH_PARENT_PROCESS);
            }
        }
    }

    // 不带参数，或显式 `ui` → 图形界面。
    if first.is_none() || first.as_deref() == Some("ui") {
        if let Err(error) = ui::run() {
            eprintln!("错误：{error:#}");
            std::process::exit(1);
        }
        return;
    }

    if let Err(error) = run(first, arguments.collect()) {
        eprintln!();
        eprintln!("错误：{error:#}");
        std::process::exit(1);
    }
}

fn run(first: Option<String>, rest: Vec<String>) -> Result<()> {
    let command = first.unwrap_or_else(|| "help".to_string());
    let mut arguments = rest.into_iter();

    match command.as_str() {
        "env" => command_env(),
        "id" => command_id(),
        "list" => command_list(),
        "usage" => command_usage(),
        "register" => command_register(),
        "import" => {
            let path = arguments.next();
            command_import(path)
        }
        "switch" => {
            let rest: Vec<String> = arguments.collect();
            command_switch(rest)
        }
        "desktop" => command_desktop(),
        "enable-seamless" | "connect-seamless" => {
            connection::ensure(&Store::discover()?, |message| println!("{message}"))
        }
        "disable-seamless" => bridge::uninstall(&Store::discover()?),
        "seamless-status" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&bridge::status(&Store::discover()?)?)?
            );
            Ok(())
        }
        "cancel-switch" => {
            for session in bridge::sessions(&Store::discover()?)? {
                session.request("cancel", None)?;
            }
            println!("已取消等待中的换号请求。");
            Ok(())
        }
        "help" | "-h" | "--help" => {
            help();
            Ok(())
        }
        other => {
            help();
            bail!("未知命令：{other}")
        }
    }
}

fn help() {
    println!(
        "codex-switch {} —— 个人版 Codex 账号切换器（Windows）",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("用法：");
    println!("  codex-switch env                          显示数据目录与活动 Codex 目录");
    println!("  codex-switch id                           读取当前 Codex 登录身份");
    println!("  codex-switch list                         列出已登记账号");
    println!("  codex-switch usage                        查询每个账号的额度（需要 codex.exe）");
    println!("  codex-switch register                     把当前登录保存为一个账号");
    println!("  codex-switch import <auth.json>           从文件导入一个账号");
    println!("  codex-switch switch <ID|邮箱> [--dry-run] 切换到指定账号");
    println!(
        "  codex-switch connect-seamless             自动连接无感后台（需要时自动重开 Codex）"
    );
    println!("  codex-switch disable-seamless             恢复启用前的 CLI 设置");
    println!("  codex-switch seamless-status              检查无感后台与等待中的切换");
    println!("  codex-switch cancel-switch                取消等待中的换号");
    println!("  codex-switch desktop                      显示 Desktop 的定位与运行状态");
}

fn command_env() -> Result<()> {
    let store = Store::discover()?;
    println!("数据目录        : {}", store.root.display());
    println!("index.json      : {}", store.index_path().display());
    println!("profiles        : {}", store.profiles_dir().display());
    println!("活动 Codex 目录 : {}", store.codex_home.display());
    println!("活动 auth.json  : {}", store.active_auth().display());
    Ok(())
}

fn command_id() -> Result<()> {
    let store = Store::discover()?;
    let identity = store.read_active_identity()?;
    println!("账号 ID  : {}", identity::mask(&identity.account_id));
    println!(
        "邮箱     : {}",
        identity.email.as_deref().unwrap_or("（凭证里没有邮箱）")
    );
    Ok(())
}

fn command_list() -> Result<()> {
    let store = Store::discover()?;
    let index = store.load()?;
    if index.accounts.is_empty() {
        println!("还没有登记任何账号。在 Codex 里登录一次后运行 register。");
        return Ok(());
    }
    for account in &index.accounts {
        let marker = if index.active.as_deref() == Some(account.id.as_str()) {
            "*"
        } else {
            " "
        };
        println!(
            "{marker} {:<32} {}",
            account.label(),
            identity::mask(&account.id)
        );
    }
    println!();
    println!("（* 表示当前账号）");
    Ok(())
}

fn command_register() -> Result<()> {
    let store = Store::discover()?;
    let account = store.register_current()?;
    println!("已登记并设为当前账号：{}", account.label());
    println!("账号 ID：{}", identity::mask(&account.id));
    Ok(())
}

fn command_import(path: Option<String>) -> Result<()> {
    let Some(path) = path else {
        bail!("用法：codex-switch import <auth.json 路径>");
    };
    let store = Store::discover()?;
    let account = store.import(Path::new(&path))?;
    println!("已导入：{}", account.label());
    if let Ok(index) = store.load() {
        if index.active.as_deref() == Some(account.id.as_str()) {
            println!("（它同时是当前账号）");
        }
    }
    Ok(())
}

fn command_switch(rest: Vec<String>) -> Result<()> {
    if rest.iter().any(|a| a.starts_with("--") && a != "--dry-run") {
        bail!("仅支持无感切号；可用参数为 --dry-run。缺少连接时会自动接通 Codex");
    }
    let dry_run = rest.iter().any(|argument| argument == "--dry-run");
    let query = rest
        .iter()
        .find(|argument| !argument.starts_with("--"))
        .cloned();
    let Some(query) = query else {
        bail!("用法：codex-switch switch <ID|邮箱> [--dry-run]");
    };
    let store = Store::discover()?;
    let outcome = switch::switch_to(&store, &query, dry_run)?;
    if !dry_run {
        if outcome.queued {
            println!("已安排切换，等待当前轮完成；窗口保持打开。可用 seamless-status 查看、cancel-switch 取消。");
        } else {
            println!("已切换，下一轮使用所选账号。");
        }
        println!("已经开着的终端 codex 进程仍是旧账号，需要新开一个进程。");
    }
    Ok(())
}

fn command_usage() -> Result<()> {
    let store = Store::discover()?;
    let index = store.load()?;
    if index.accounts.is_empty() {
        println!("还没有登记任何账号。");
        return Ok(());
    }

    let located = codex::locate();
    let Some(codex_path) = located else {
        println!("没有找到 codex.exe，无法查询额度。");
        println!("查找情况：{}", codex::describe_search());
        println!("可以设置环境变量 CODEX_CLI_PATH 指向 codex.exe 后重试。");
        return Ok(());
    };
    println!("codex.exe：{}", codex_path.display());
    println!();

    for account in &index.accounts {
        let profile = store.profile_auth(&account.id);
        let Some(home) = profile.parent() else {
            continue;
        };
        let parsed = std::fs::read(&profile)
            .ok()
            .and_then(|bytes| identity::parse_auth_json(&bytes).ok());

        println!("{}", account.label());
        println!(
            "  订阅      : {}",
            parsed
                .as_ref()
                .and_then(|value| value.plan.as_deref())
                .unwrap_or("—")
        );
        println!(
            "  有效期    : {}",
            parsed
                .as_ref()
                .and_then(|value| value.active_until.as_deref())
                .unwrap_or("—")
        );

        match usage::read(&codex_path, home) {
            Ok(value) => {
                println!(
                    "  5 小时    : {}",
                    describe_usage(value.five_hour_remaining, value.five_hour_resets_at, true)
                );
                println!(
                    "  每周      : {}",
                    describe_usage(value.weekly_remaining, value.weekly_resets_at, false)
                );
                println!(
                    "  可重置次数: {}",
                    value
                        .reset_credits
                        .map(|count| count.to_string())
                        .unwrap_or_else(|| "—".to_string())
                );
            }
            Err(error) => println!("  额度读取失败：{error:#}"),
        }
        println!();
    }
    Ok(())
}

fn describe_usage(remaining: Option<u8>, resets_at: Option<i64>, with_time: bool) -> String {
    match (remaining, resets_at) {
        (Some(percent), Some(epoch)) => {
            format!("剩余 {percent}%（{} 重置）", format_reset(epoch, with_time))
        }
        (Some(percent), None) => format!("剩余 {percent}%"),
        _ => "—".to_string(),
    }
}

fn format_reset(epoch: i64, with_time: bool) -> String {
    match win::local_datetime(epoch) {
        Some((_year, month, day, hour, minute)) => {
            if with_time {
                format!("{hour:02}:{minute:02}")
            } else {
                format!("{month}月{day}日")
            }
        }
        None => "—".to_string(),
    }
}

fn command_desktop() -> Result<()> {
    match desktop::Desktop::discover() {
        Ok(desktop) => {
            println!("可执行文件 : {}", desktop.installation.executable.display());
            println!(
                "AUMID      : {}",
                desktop
                    .installation
                    .aumid
                    .as_deref()
                    .unwrap_or("（不是 Store 安装）")
            );
            let running = desktop.running();
            if running.is_empty() {
                println!("运行状态   : 未运行");
            } else {
                println!("运行状态   : 运行中");
                for process in running {
                    println!("  pid {}  {}", process.pid, process.path.display());
                }
            }
        }
        Err(error) => println!("未定位到 Codex Desktop：{error:#}"),
    }
    Ok(())
}
