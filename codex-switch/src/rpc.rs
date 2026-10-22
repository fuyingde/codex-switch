//! `codex app-server --stdio` 的最小 JSON-RPC 客户端。
//!
//! 协议是按行分隔的 JSON：请求带 `id`，响应带同一个 `id`；没有 `id` 的是通知。
//! 打开一个 profile 目录作为 `CODEX_HOME` 就能查询那个账号的额度，不需要切换账号。
//! app-server 会在 `CODEX_HOME` 下建 sqlite 状态库，所以那个目录必须可写。

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct AppServer {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    next_id: i64,
    timeout: Duration,
    notifications: Vec<Value>,
}

impl AppServer {
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    pub fn start(executable: &Path, codex_home: &Path) -> Result<Self> {
        Self::start_with_overrides(executable, codex_home, &[])
    }

    /// Overrides belong only to this child; never rewrite the user's Codex config.
    pub fn start_with_overrides(
        executable: &Path,
        codex_home: &Path,
        overrides: &[String],
    ) -> Result<Self> {
        let mut command = Command::new(executable);
        for value in overrides {
            command.arg("-c").arg(value);
        }
        command
            .arg("app-server")
            .arg("--stdio")
            .env("CODEX_HOME", codex_home)
            // 不弹控制台窗口
            .creation_flags(crate::win::CREATE_NO_WINDOW)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .with_context(|| format!("无法启动 codex app-server：{}", executable.display()))?;

        let stdout = child.stdout.take().context("app-server 没有 stdout")?;
        let stderr = child.stderr.take().context("app-server 没有 stderr")?;
        let stdin = child.stdin.take().context("app-server 没有 stdin")?;

        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // stderr 必须持续读走（否则管道写满会卡死子进程），同时留最后几十行做错误说明。
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let tail = Arc::clone(&stderr_tail);
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if let Ok(mut buffer) = tail.lock() {
                    buffer.push_back(line);
                    if buffer.len() > 40 {
                        buffer.pop_front();
                    }
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            lines,
            stderr_tail,
            next_id: 1,
            timeout: Duration::from_secs(20),
            notifications: Vec::new(),
        })
    }

    pub fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            serde_json::json!({
                "clientInfo": {
                    "name": "codex-switch",
                    "title": "Codex Switch",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }),
        )?;
        self.send(&serde_json::json!({ "method": "initialized", "params": {} }))
    }

    pub fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&serde_json::json!({ "id": id, "method": method, "params": params }))?;

        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("等待 {method} 超时{}", self.stderr_suffix());
            }
            let message = self.receive(remaining)?;
            if message.get("id").and_then(Value::as_i64) == Some(id) {
                if let Some(error) = message.get("error") {
                    let text = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("未知错误");
                    bail!("{method} 失败：{text}");
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            if message.get("method").is_some() && message.get("id").is_none() {
                self.notifications.push(message);
                if self.notifications.len() > 32 {
                    self.notifications.remove(0);
                }
            }
        }
    }

    /// 等待某个通知（例如 `account/login/completed`），支持取消标志，每 400ms 检查一次。
    pub fn wait_notification_cancellable(
        &mut self,
        method: &str,
        timeout: Duration,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<Value> {
        use std::sync::atomic::Ordering;

        let deadline = Instant::now() + timeout;
        loop {
            if let Some(index) = self
                .notifications
                .iter()
                .position(|value| value.get("method").and_then(Value::as_str) == Some(method))
            {
                return Ok(self.notifications.remove(index));
            }
            if let Some(flag) = cancel {
                if flag.load(Ordering::Relaxed) {
                    bail!("已取消");
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("等待 {method} 超时{}", self.stderr_suffix());
            }
            match self
                .lines
                .recv_timeout(remaining.min(Duration::from_millis(400)))
            {
                Ok(line) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                        continue;
                    };
                    if value.get("method").and_then(Value::as_str) == Some(method) {
                        return Ok(value);
                    }
                    if value.get("method").is_some() && value.get("id").is_none() {
                        self.notifications.push(value);
                        if self.notifications.len() > 32 {
                            self.notifications.remove(0);
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("app-server 提前退出{}", self.stderr_suffix())
                }
            }
        }
    }

    fn send(&mut self, value: &Value) -> Result<()> {
        let mut text = serde_json::to_string(value)?;
        text.push('\n');
        self.stdin
            .write_all(text.as_bytes())
            .context("写入 app-server 失败")?;
        self.stdin.flush().context("刷新 app-server stdin 失败")
    }

    fn receive(&mut self, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("等待 app-server 响应超时{}", self.stderr_suffix());
            }
            match self.lines.recv_timeout(remaining) {
                Ok(line) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    // app-server 偶尔会在 stdout 打日志，跳过解析不了的行。
                    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
                        return Ok(value);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    bail!("等待 app-server 响应超时{}", self.stderr_suffix())
                }
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("app-server 提前退出{}", self.stderr_suffix())
                }
            }
        }
    }

    fn stderr_suffix(&self) -> String {
        let text = self
            .stderr_tail
            .lock()
            .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        let text = text.trim();
        if text.is_empty() {
            String::new()
        } else {
            format!("：{text}")
        }
    }
}

impl Drop for AppServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
