//! Transparent Desktop stdio bridge. Owns the idle boundary and account verification.
//! No Codex installation files are patched. Non app-server commands pass through unchanged.

use crate::{codex, identity, rpc::AppServer, store::Store, switch::Prepared, win};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

const MAX_CONTROL: u64 = 64 * 1024;
const RPC_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Serialize, Deserialize)]
struct Installation {
    version: u32,
    real_cli: PathBuf,
    previous_cli_override: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    version: u32,
    proxy_pid: u32,
    home: PathBuf,
    port: u16,
    capability: String,
}

impl Session {
    pub fn request(&self, method: &str, account: Option<&str>) -> Result<Value> {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(500))
            .context("无感后台连接已断开；账号未被自动切换")?;
        stream.set_read_timeout(Some(Duration::from_secs(if method == "status" {
            2
        } else {
            45
        })))?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        serde_json::to_writer(
            &mut stream,
            &json!({
                "capability": self.capability, "method": method, "account": account,
            }),
        )?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        let response = read_control(&mut stream)?;
        if response.get("ok").and_then(Value::as_bool) != Some(true) {
            bail!(
                "{}",
                response
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("无感后台拒绝请求")
            );
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }
}

fn same_home(a: &Path, b: &Path) -> bool {
    let normalize = |p: &Path| {
        p.canonicalize()
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    normalize(a) == normalize(b)
}

pub fn sessions(store: &Store) -> Result<Vec<Session>> {
    Ok(live_sessions(store)?
        .into_iter()
        .filter(|s| {
            s.request("status", None)
                .ok()
                .is_some_and(|v| v["ready"] == true)
        })
        .collect())
}

pub(crate) fn live_sessions(store: &Store) -> Result<Vec<Session>> {
    Ok(registered_sessions(store)?
        .into_iter()
        .filter(|session| session.request("status", None).is_ok())
        .collect())
}

/// A slow/busy bridge is still registered. Never equate a probe timeout with absence.
pub(crate) fn registered_sessions(store: &Store) -> Result<Vec<Session>> {
    let directory = store.root.join("bridge/sessions");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    let live: HashMap<u32, Option<PathBuf>> = win::processes()
        .into_iter()
        .map(|p| (p.pid, p.path))
        .collect();
    for entry in std::fs::read_dir(directory)?.flatten() {
        if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok(session) = serde_json::from_slice::<Session>(&bytes) else {
            continue;
        };
        let process = live.get(&session.proxy_pid);
        let reused = process.and_then(|p| p.as_ref()).is_some_and(|p| {
            !codex::is_bridge(p)
                && std::env::current_exe()
                    .ok()
                    .is_none_or(|current| !same_home(p, &current))
        });
        if process.is_none() || reused {
            if entry.file_name().to_string_lossy() == format!("{}.json", session.proxy_pid) {
                let _ = std::fs::remove_file(entry.path());
            }
            continue;
        }
        if session.version != 1 || !same_home(&session.home, &store.codex_home) {
            continue;
        }
        result.push(session);
    }
    Ok(result)
}

pub fn status(store: &Store) -> Result<Value> {
    let sessions = live_sessions(store)?;
    let statuses = sessions
        .iter()
        .map(|s| s.request("status", None))
        .collect::<Result<Vec<_>>>()?;
    let installed = store.root.join("bridge/install.json").exists()
        && read_user_override()?.is_some_and(|s| managed_wrapper(store, Path::new(&s)));
    Ok(json!({ "installed": installed, "sessions": statuses }))
}

pub fn wait_for_switch(store: &Store, account: &str) -> Result<bool> {
    loop {
        let sessions = sessions(store)?;
        if sessions.len() != 1 {
            bail!("等待期间无感后台断开，请重新检查当前账号");
        }
        let status = sessions[0].request("status", None)?;
        if status.get("pending").and_then(Value::as_str).is_none() {
            if let Some(error) = status.get("lastError").and_then(Value::as_str) {
                bail!("{error}");
            }
            if status.get("active").and_then(Value::as_str) != Some(account) {
                return Ok(false);
            }
            return Ok(true);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Content-addressed runtimes let a new EXE install while the old bridge is running.
pub(crate) fn build_id() -> Result<String> {
    let bytes = std::fs::read(std::env::current_exe()?)?;
    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x100000001b3)
    });
    Ok(format!("{hash:016x}"))
}

fn managed_wrapper(store: &Store, path: &Path) -> bool {
    codex::is_bridge(path)
        && path.parent().is_some_and(|p| {
            same_home(p, &store.root.join("bridge"))
                || p.parent()
                    .is_some_and(|parent| same_home(parent, &store.root.join("bridge")))
        })
}

pub fn install(store: &Store) -> Result<PathBuf> {
    let directory = store.root.join("bridge");
    std::fs::create_dir_all(&directory)?;
    win::protect_private_path(&directory)?;
    let config_path = directory.join("install.json");
    let existing = std::fs::read(&config_path)
        .ok()
        .map(|b| serde_json::from_slice::<Installation>(&b))
        .transpose()?;
    let previous = read_user_override()?;
    let runtime = directory.join(format!("runtime-{}", build_id()?));
    std::fs::create_dir_all(&runtime)?;
    win::protect_private_path(&runtime)?;
    let wrapper = runtime.join("codex-switch-bridge.exe");
    let previous_cli_override = existing
        .as_ref()
        .map(|c| c.previous_cli_override.clone())
        .unwrap_or_else(|| previous.clone());
    if existing.is_some()
        && previous
            .as_deref()
            .is_some_and(|p| !managed_wrapper(store, Path::new(p)))
        && previous != previous_cli_override
    {
        bail!("用户 CLI 设置已被其他程序修改，未覆盖；请先确认 CODEX_CLI_PATH 设置");
    }
    let real_cli = std::env::var_os("CODEX_SWITCH_REAL_CLI")
        .map(PathBuf::from)
        .or_else(|| {
            previous_cli_override
                .as_ref()
                .map(PathBuf::from)
                .filter(|p| p.is_file() && !codex::is_bridge(p))
        })
        .or_else(codex::desktop_cli)
        .or_else(|| existing.as_ref().map(|i| i.real_cli.clone()))
        .context("找不到原版 codex.exe，未更改环境设置")?;
    if codex::is_bridge(&real_cli) {
        bail!("原版 CLI 不能指向无感桥接器自身");
    }
    let source = std::env::current_exe()?;
    if !same_home(&source, &wrapper) {
        // Windows locks a running executable. Re-enabling the same build needs no replacement.
        let same =
            std::fs::read(&wrapper).ok().as_deref() == Some(std::fs::read(&source)?.as_slice());
        if !same {
            std::fs::copy(&source, &wrapper).context("无法安装无感连接程序")?;
        }
    }
    let config = Installation {
        version: 1,
        real_cli,
        previous_cli_override,
    };
    win::write_atomic(&config_path, &serde_json::to_vec_pretty(&config)?)?;
    win::protect_private_path(&config_path)?;
    if previous
        .as_deref()
        .is_none_or(|p| !same_home(Path::new(p), &wrapper))
    {
        write_user_override(Some(wrapper.to_string_lossy().as_ref()))?;
    }
    std::env::set_var("CODEX_CLI_PATH", &wrapper);
    Ok(wrapper)
}

pub fn uninstall(store: &Store) -> Result<()> {
    let directory = store.root.join("bridge");
    let config: Installation = serde_json::from_slice(
        &std::fs::read(directory.join("install.json")).context("尚未安装无感后台")?,
    )?;
    let current = read_user_override()?;
    if current
        .as_deref()
        .is_some_and(|s| managed_wrapper(store, Path::new(s)))
    {
        write_user_override(config.previous_cli_override.as_deref())?;
    } else if current != config.previous_cli_override {
        bail!("CODEX_CLI_PATH 已被其他设置修改，未覆盖该设置");
    }
    // A running bridge may still own the executable; retain it until Desktop exits normally.
    println!("已恢复启用前的 CLI 设置；当前窗口可继续使用，下次打开 Codex 后恢复原模式。");
    Ok(())
}

fn read_user_override() -> Result<Option<String>> {
    if let Some(path) = std::env::var_os("CODEX_SWITCH_SETTINGS_FILE") {
        return match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        };
    }
    let output = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command",
        "[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); $v=[Environment]::GetEnvironmentVariable('CODEX_CLI_PATH','User'); if($null -eq $v){Write-Output 'null'}else{ConvertTo-Json -InputObject $v -Compress}"])
        .creation_flags(win::CREATE_NO_WINDOW).output()?;
    if !output.status.success() {
        bail!("无法读取用户 CLI 设置");
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn write_user_override(value: Option<&str>) -> Result<()> {
    if let Some(path) = std::env::var_os("CODEX_SWITCH_SETTINGS_FILE") {
        return win::write_atomic(Path::new(&path), &serde_json::to_vec(&value)?);
    }
    let output = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command",
        "$ErrorActionPreference='Stop'; [Environment]::SetEnvironmentVariable('CODEX_CLI_PATH', $(if($env:CODEX_SWITCH_REMOVE_OVERRIDE -eq '1'){$null}else{$env:CODEX_SWITCH_NEW_OVERRIDE}), 'User')"])
        .env("CODEX_SWITCH_NEW_OVERRIDE", value.unwrap_or(""))
        .env("CODEX_SWITCH_REMOVE_OVERRIDE", if value.is_none() { "1" } else { "0" })
        .creation_flags(win::CREATE_NO_WINDOW).output()?;
    if !output.status.success() {
        bail!("无法保存用户 CLI 设置");
    }
    // Notify Explorer and other launchers that user environment variables changed.
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
        };
        let text: Vec<u16> = "Environment\0".encode_utf16().collect();
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            text.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            1000,
            std::ptr::null_mut(),
        );
    }
    Ok(())
}

fn read_control(stream: &mut TcpStream) -> Result<Value> {
    let mut bytes = Vec::new();
    let count = BufReader::new(stream)
        .take(MAX_CONTROL + 1)
        .read_until(b'\n', &mut bytes)?;
    if count == 0 || count as u64 > MAX_CONTROL || bytes.last() != Some(&b'\n') {
        bail!("无效的本地请求");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

enum Event {
    Input(Value),
    Output(Value),
    Control(Value, TcpStream),
    InputClosed,
    OutputClosed,
}

struct Proxy {
    child: Child,
    input: ChildStdin,
    rx: Receiver<Event>,
    deferred: VecDeque<Event>,
    held_turns: VecDeque<Value>,
    store: Store,
    real_cli: PathBuf,
    ready: bool,
    initialize_id: Option<Value>,
    build: String,
    active: HashSet<String>,
    starts: HashMap<String, String>,
    pending: Option<String>,
    last_error: Option<String>,
    external_account: Option<String>,
    external_token: Option<String>,
    applying: bool,
    next_id: u64,
    session_path: PathBuf,
}

pub fn run_proxy(arguments: Vec<String>) -> Result<i32> {
    let store = Store::discover()?;
    let configured = std::fs::read(store.root.join("bridge/install.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Installation>(&b).ok());
    let saved_override = configured
        .as_ref()
        .filter(|c| c.previous_cli_override.is_some())
        .map(|c| c.real_cli.clone());
    let real_cli = std::env::var_os("CODEX_SWITCH_REAL_CLI")
        .map(PathBuf::from)
        .or(saved_override)
        .or_else(codex::desktop_cli)
        .or_else(|| configured.map(|c| c.real_cli))
        .context("找不到原版 codex.exe，请重新启用无感换号")?;
    if codex::is_bridge(&real_cli) {
        bail!("拒绝递归启动桥接器");
    }
    let serve = arguments.iter().any(|a| a == "app-server")
        && !arguments.iter().any(|a| {
            matches!(
                a.as_str(),
                "generate-ts" | "generate-json-schema" | "daemon" | "proxy"
            )
        })
        && !arguments.iter().any(|a| a == "--listen");
    let mut command = Command::new(&real_cli);
    command
        .args(&arguments)
        .env("CODEX_CLI_PATH", &real_cli)
        .creation_flags(win::CREATE_NO_WINDOW);
    if !serve {
        return Ok(command.status()?.code().unwrap_or(1));
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = command.spawn().context("原版 Codex 后台启动失败")?;
    let input = child.stdin.take().context("后台输入管道不可用")?;
    let output = child.stdout.take().context("后台输出管道不可用")?;
    let (tx, rx) = mpsc::channel();
    let output_tx = tx.clone();
    std::thread::spawn(move || pump(BufReader::new(output), output_tx, false));
    let input_tx = tx.clone();
    std::thread::spawn(move || pump(BufReader::new(std::io::stdin()), input_tx, true));
    let listener =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).context("无法绑定本机无感控制端口")?;
    let session = Session {
        version: 1,
        proxy_pid: std::process::id(),
        home: store.codex_home.clone(),
        port: listener.local_addr()?.port(),
        capability: random_capability()?,
    };
    let directory = store.root.join("bridge/sessions");
    std::fs::create_dir_all(&directory).context("无法创建无感会话目录")?;
    win::protect_private_path(&directory)?;
    let session_path = directory.join(format!("{}.json", std::process::id()));
    win::write_atomic(&session_path, &serde_json::to_vec(&session)?)?;
    win::protect_private_path(&session_path)?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            let capability = session.capability.clone();
            std::thread::spawn(move || {
                let mut stream = stream;
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
                let Ok(value) = read_control(&mut stream) else {
                    return;
                };
                if value.get("capability").and_then(Value::as_str) != Some(capability.as_str()) {
                    return;
                }
                let _ = tx.send(Event::Control(value, stream));
            });
        }
    });
    let mut proxy = Proxy {
        child,
        input,
        rx,
        deferred: VecDeque::new(),
        held_turns: VecDeque::new(),
        store,
        real_cli,
        ready: false,
        initialize_id: None,
        build: build_id()?,
        active: HashSet::new(),
        starts: HashMap::new(),
        pending: None,
        last_error: None,
        external_account: None,
        external_token: None,
        applying: false,
        next_id: 0,
        session_path,
    };
    proxy.run()
}

fn random_capability() -> Result<String> {
    use windows_sys::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    let mut bytes = [0u8; 32];
    if unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    } < 0
    {
        bail!("无法生成本地桥接访问密钥");
    }
    use base64::Engine;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

fn pump(reader: impl BufRead, tx: Sender<Event>, is_input: bool) {
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Ok(value) = serde_json::from_str(&line) else {
            continue;
        };
        let event = if is_input {
            Event::Input(value)
        } else {
            Event::Output(value)
        };
        if tx.send(event).is_err() {
            return;
        }
    }
    let _ = tx.send(if is_input {
        Event::InputClosed
    } else {
        Event::OutputClosed
    });
}

fn output(value: &Value) -> Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer(&mut lock, value).context("无法向 Desktop 写入后台协议响应")?;
    lock.write_all(b"\n").context("无法结束后台协议响应")?;
    lock.flush().context("无法刷新后台协议响应")?;
    Ok(())
}

impl Proxy {
    fn send(&mut self, value: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.input, value)?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        Ok(())
    }

    fn run(&mut self) -> Result<i32> {
        loop {
            let event = self.deferred.pop_front().or_else(|| self.rx.recv().ok());
            match event {
                Some(Event::Input(mut value)) => {
                    match value.get("method").and_then(Value::as_str) {
                        Some("initialize") => {
                            self.initialize_id = value.get("id").cloned();
                            let params = value
                                .get_mut("params")
                                .and_then(Value::as_object_mut)
                                .context("初始化参数无效")?;
                            let capabilities = params.entry("capabilities").or_insert(json!({}));
                            capabilities
                                .as_object_mut()
                                .context("初始化能力参数无效")?
                                .insert("experimentalApi".into(), json!(true));
                        }
                        // Desktop may omit this optional notification. Readiness is tied to
                        // the successful initialize response, never to the notification.
                        Some("initialized") => {}
                        Some("account/login/start" | "account/logout") => {
                            self.external_account = None;
                            self.external_token = None;
                            self.pending = None;
                        }
                        Some("turn/start") => {
                            // Other chats may finish earlier. Their next turn must wait for the
                            // same account boundary instead of silently starting on the old account.
                            if self.pending.is_some() {
                                if self.held_turns.len() >= 64 {
                                    self.reject_turn(&value, "等待换号的请求过多，请稍后重试")?;
                                } else {
                                    self.held_turns.push_back(value);
                                }
                                continue;
                            }
                            // Refresh a managed profile before the child needs an external-token callback.
                            if let Some(id) = self.external_account.clone() {
                                match self.tokens(&id, false) {
                                    Ok(tokens) => {
                                        // A usage refresh may have rotated the access token in this profile.
                                        if self.active.is_empty()
                                            && tokens.get("accessToken").and_then(Value::as_str)
                                                != self.external_token.as_deref()
                                        {
                                            if let Err(error) = self.login(&tokens) {
                                                self.reject_turn(
                                                    &value,
                                                    &format!("更新当前账号凭据失败：{error:#}"),
                                                )?;
                                                continue;
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        self.reject_turn(
                                            &value,
                                            &format!("当前账号需要重新登录：{error:#}"),
                                        )?;
                                        continue;
                                    }
                                }
                            }
                            if let (Some(id), Some(thread)) = (
                                value.get("id"),
                                value.pointer("/params/threadId").and_then(Value::as_str),
                            ) {
                                if self.active.insert(thread.into()) {
                                    self.starts.insert(id.to_string(), thread.into());
                                }
                            }
                        }
                        _ => {}
                    }
                    self.send(&value)?;
                }
                Some(Event::Output(value)) => {
                    self.forward(value)?;
                }
                Some(Event::Control(value, mut stream)) => {
                    let result = match value.get("method").and_then(Value::as_str) {
                        Some("status") => Ok(self.status_value()),
                        Some("switch") => self
                            .schedule(value.get("account").and_then(Value::as_str).unwrap_or("")),
                        Some("cancel") => {
                            self.pending = None;
                            Ok(json!({"state":"cancelled"}))
                        }
                        _ => Err(anyhow::anyhow!("未知的本地操作")),
                    };
                    let response = match result {
                        Ok(value) => json!({"ok":true,"result":value}),
                        Err(error) => json!({"ok":false,"error":format!("{error:#}")}),
                    };
                    let _ = serde_json::to_writer(&mut stream, &response);
                    let _ = stream.write_all(b"\n");
                }
                Some(Event::InputClosed) | None => return Ok(0),
                Some(Event::OutputClosed) => return Ok(self.child.wait()?.code().unwrap_or(1)),
            }
            if self.ready && self.active.is_empty() {
                if let Some(id) = self.pending.take() {
                    self.last_error = self.apply(&id).err().map(|e| format!("{e:#}"));
                }
            }
            if self.pending.is_none() {
                while let Some(value) = self.held_turns.pop_back() {
                    if let Some(error) = &self.last_error {
                        self.reject_turn(&value, &format!("排队换号失败，本轮未发送：{error}"))?;
                    } else {
                        self.deferred.push_front(Event::Input(value));
                    }
                }
            }
        }
    }

    fn reject_turn(&self, request: &Value, message: &str) -> Result<()> {
        output(&json!({"id":request.get("id"),"error":{"code":-32000,"message":message}}))
    }

    fn status_value(&self) -> Value {
        json!({"ready":self.ready,"protocol":1,"proxyPid":std::process::id(),"backendPid":self.child.id(),
            "build":self.build,"version":env!("CARGO_PKG_VERSION"),
            "activeTurns":self.active.len(),"pending":self.pending,"lastError":self.last_error,
            "active":self.store.load().ok().and_then(|i| i.active)})
    }

    fn schedule(&mut self, id: &str) -> Result<Value> {
        if !self.ready {
            bail!("Desktop 后台尚未完成初始化，账号未改动");
        }
        Prepared::load(&self.store, id)?;
        if self.pending.is_some() {
            bail!("已有换号请求正在等待当前轮完成，请等待或先取消");
        }
        self.last_error = None;
        if !self.active.is_empty() {
            self.pending = Some(id.to_string());
            return Ok(json!({"state":"queued"}));
        }
        if let Err(error) = self.apply(id) {
            self.last_error = Some(format!("{error:#}"));
            return Err(error);
        }
        Ok(json!({"state":"applied"}))
    }

    fn forward(&mut self, value: Value) -> Result<()> {
        // A timed-out internal RPC can arrive late; never expose its auth token to the frontend.
        if value
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with(&format!("codex-switch:{}:", std::process::id())))
        {
            return Ok(());
        }
        if value.get("method").and_then(Value::as_str) == Some("account/chatgptAuthTokens/refresh")
        {
            let expected = value
                .pointer("/params/previousAccountId")
                .and_then(Value::as_str);
            let response = match self
                .external_account
                .clone()
                .context("没有已选择的外部账号")
                .and_then(|id| {
                    if expected.is_some_and(|previous| previous != id) {
                        bail!("认证刷新账号不匹配");
                    }
                    self.tokens(&id, true)
                }) {
                Ok(tokens) => {
                    self.external_token = tokens
                        .get("accessToken")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    json!({"id":value.get("id"),"result":tokens})
                }
                Err(_) => {
                    json!({"id":value.get("id"),"error":{"code":-32000,"message":"当前账号凭据刷新失败，请重新登录；未自动换成其他账号"}})
                }
            };
            return self.send(&response);
        }
        self.observe(&value);
        output(&value)
    }

    fn observe(&mut self, value: &Value) {
        if self
            .initialize_id
            .as_ref()
            .is_some_and(|id| value.get("id") == Some(id))
        {
            self.ready = value.get("result").is_some() && value.get("error").is_none();
            self.initialize_id = None;
        }
        if value.get("method").and_then(Value::as_str) == Some("account/updated")
            && value.pointer("/params/authMode").and_then(Value::as_str)
                != Some("chatgptAuthTokens")
        {
            self.external_account = None;
            self.external_token = None;
        }
        if let Some(id) = value.get("id") {
            if let Some(thread) = self.starts.remove(&id.to_string()) {
                if value.get("error").is_some() {
                    self.active.remove(&thread);
                }
            }
        }
        let thread = value.pointer("/params/threadId").and_then(Value::as_str);
        match (value.get("method").and_then(Value::as_str), thread) {
            (Some("turn/started"), Some(id)) => {
                self.active.insert(id.into());
            }
            (Some("turn/completed" | "thread/closed"), Some(id)) => {
                self.active.remove(id);
            }
            _ => {}
        }
    }

    /// Internal responses are consumed here; unrelated Desktop output is never stolen.
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = format!("codex-switch:{}:{}", std::process::id(), self.next_id);
        self.send(&json!({"id":id,"method":method,"params":params}))?;
        let deadline = Instant::now() + RPC_TIMEOUT;
        loop {
            let event = self
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .context("等待后台认证更新超时，请检查无感状态；不会自动重启窗口")?;
            match event {
                Event::Output(value)
                    if value.get("id").and_then(Value::as_str) == Some(id.as_str()) =>
                {
                    if value.get("error").is_some() {
                        bail!("后台拒绝认证更新（{method}）；请检查账号是否有效及版本是否兼容");
                    }
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
                Event::Output(value) => self.forward(value)?,
                Event::OutputClosed => bail!("认证更新期间后台退出"),
                event => self.deferred.push_back(event),
            }
        }
    }

    fn login(&mut self, tokens: &Value) -> Result<()> {
        let account = tokens
            .get("chatgptAccountId")
            .and_then(Value::as_str)
            .context("后台账号参数无效")?
            .to_owned();
        // Refresh callbacks can arrive before account/login/start returns.
        self.external_account = Some(account.clone());
        let mut params = tokens.clone();
        params
            .as_object_mut()
            .context("凭据格式无效")?
            .insert("type".into(), json!("chatgptAuthTokens"));
        self.call("account/login/start", params)?;
        let actual = self.call(
            "getAuthStatus",
            json!({"includeToken":true,"refreshToken":false}),
        )?;
        let actual_token = actual
            .get("authToken")
            .and_then(Value::as_str)
            .unwrap_or("");
        let saved = std::fs::read(self.store.profile_auth(&account))
            .ok()
            .and_then(|b| token_params(&b).ok());
        let known_token = actual.get("authToken") == tokens.get("accessToken")
            || saved.as_ref().and_then(|v| v.get("accessToken")) == actual.get("authToken");
        if !known_token
            || token_account(actual_token).as_deref() != Some(&account)
            || actual.get("authMethod").and_then(Value::as_str) != Some("chatgptAuthTokens")
        {
            bail!("后台账号校验失败，拒绝报告切换成功");
        }
        self.external_token = Some(actual_token.to_owned());
        Ok(())
    }

    fn apply(&mut self, id: &str) -> Result<()> {
        let _ = self.tokens(id, false)?; // Refresh expired target credentials before snapshotting.
        let prepared = Prepared::load(&self.store, id)?;
        let mut previous = token_params(&prepared.previous_bytes)?;
        let current = self.call(
            "getAuthStatus",
            json!({"includeToken":true,"refreshToken":false}),
        )?;
        let current_token = current
            .get("authToken")
            .and_then(Value::as_str)
            .context("后台没有可切换的 ChatGPT 登录")?;
        let old_identity = identity::parse_auth_json(&prepared.previous_bytes)?;
        if token_account(current_token).as_deref() != Some(&old_identity.account_id) {
            bail!("后台账号与磁盘凭据不一致，账号未改动；请先在切号器登记当前登录");
        }
        previous["accessToken"] = json!(current_token);
        let previous_account = old_identity.account_id;
        let target = token_params(&prepared.target_bytes)?;
        self.applying = true;
        let result = self
            .login(&target)
            .and_then(|_| Prepared::load(&self.store, id)?.commit(&self.store));
        if let Err(error) = result {
            let restored = self.login(&previous);
            self.applying = false;
            restored.context("切换失败，恢复后台账号也失败；请检查当前登录状态")?;
            self.external_account = Some(previous_account);
            return Err(error);
        }
        self.applying = false;
        self.external_account = Some(id.to_owned());
        Ok(())
    }

    fn tokens(&mut self, id: &str, force: bool) -> Result<Value> {
        let path = self.store.profile_auth(id);
        let mut bytes = std::fs::read(&path).context("无法读取已保存的账号凭据")?;
        let identity = identity::parse_auth_json(&bytes)?;
        if identity.account_id != id {
            bail!("保存的账号身份不一致");
        }
        let value = token_params(&bytes)?;
        let expires = value
            .get("accessToken")
            .and_then(Value::as_str)
            .and_then(identity::jwt_payload)
            .and_then(|v| v.get("exp").and_then(Value::as_i64));
        if force || expires.is_none_or(|e| e < crate::store::now_seconds() + 120) {
            let mut server =
                AppServer::start(&self.real_cli, path.parent().context("账号目录无效")?)?;
            server.set_timeout(Duration::from_secs(7));
            server.initialize()?;
            let refreshed_status = server.request(
                "getAuthStatus",
                json!({"includeToken":true,"refreshToken":true}),
            )?;
            bytes = std::fs::read(&path)?;
            let refreshed = identity::parse_auth_json(&bytes)?;
            if refreshed.account_id != id {
                bail!("凭据刷新后账号不一致");
            }
            let refreshed_params = token_params(&bytes)?;
            if refreshed_status.get("authToken") != refreshed_params.get("accessToken")
                || refreshed_status
                    .get("authToken")
                    .and_then(Value::as_str)
                    .is_none()
            {
                bail!("账号凭据刷新失败，请重新登录该账号");
            }
            if !self.applying && self.external_account.as_deref() == Some(id) {
                let active = self.store.read_active_identity()?;
                if active.account_id != id {
                    bail!("当前登录已在外部改变，停止刷新");
                }
                win::write_atomic(&self.store.active_auth(), &bytes)?;
            }
        }
        token_params(&bytes)
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.session_path);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn token_account(token: &str) -> Option<String> {
    let claims = identity::jwt_payload(token)?;
    claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn token_params(bytes: &[u8]) -> Result<Value> {
    let identity = identity::parse_auth_json(bytes)?;
    let value: Value =
        serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))?;
    let token = value
        .pointer("/tokens/access_token")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .context("账号缺少 access_token，请重新登录")?;
    if token_account(token).as_deref() != Some(&identity.account_id) {
        bail!("access_token 与账号身份不一致，拒绝切换");
    }
    Ok(
        json!({"accessToken":token,"chatgptAccountId":identity.account_id,"chatgptPlanType":identity.plan}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_mismatched_access_token() {
        use base64::Engine;
        let claim = json!({"https://api.openai.com/auth":{"chatgpt_account_id":"other"}});
        let token = format!(
            "e30.{}.sig",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claim.to_string())
        );
        let auth = json!({"tokens":{"account_id":"expected","access_token":token}});
        assert!(token_params(auth.to_string().as_bytes()).is_err());
    }
    #[test]
    fn home_comparison_is_case_insensitive() {
        assert!(same_home(
            Path::new(r"C:\Users\Test\.codex\"),
            Path::new(r"c:\users\test\.codex")
        ));
    }
}
