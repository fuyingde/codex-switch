//! Update checks and executable staging. No account/bridge/desktop operations.
#[path = "update_http.rs"]
mod http;
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const METADATA_URL: &str = "https://www.imtr.cn/codex-switch/data.json";
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(5);
pub const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub [u16; 4]);
impl Version {
    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim().strip_prefix('v').unwrap_or(value.trim());
        let parts: Vec<_> = value.split('.').collect();
        ensure!((2..=4).contains(&parts.len()), "版本号格式错误");
        let mut result = [0u16; 4];
        for (i, part) in parts.iter().enumerate() {
            ensure!(
                !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit()),
                "版本号格式错误"
            );
            result[i] = part.parse().context("版本号格式错误")?;
        }
        Ok(Self(result))
    }
    pub fn key(&self) -> String {
        self.0
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }
}

pub fn parse_metadata(bytes: &[u8]) -> Result<String> {
    #[derive(Deserialize)]
    struct Metadata {
        version: String,
    }
    let data: Metadata =
        serde_json::from_slice(bytes.strip_prefix(&[239, 187, 191]).unwrap_or(bytes))
            .context("版本数据无效")?;
    Version::parse(&data.version)?;
    Ok(data.version.trim().trim_start_matches('v').into())
}
pub fn check() -> Result<String> {
    let mut bytes = Vec::new();
    let path = format!(
        "/codex-switch/data.json?v={}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()
    );
    debug_assert_eq!(METADATA_URL, "https://www.imtr.cn/codex-switch/data.json");
    http::get(&path, CHECK_TIMEOUT, 65536, &mut bytes)?;
    parse_metadata(&bytes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Idle,
    Checking,
    Latest,
    Failed,
}
pub struct CheckJob {
    pub started: Instant,
    pub manual: bool,
    receiver: Receiver<(Instant, Result<String, String>)>,
}
pub struct Checker {
    pub last_auto: Instant,
    pub status: Status,
    pub job: Option<CheckJob>,
    pub pending: Option<(String, bool)>,
    pub ignored: Vec<String>,
}
impl Default for Checker {
    fn default() -> Self {
        Self {
            last_auto: Instant::now(),
            status: Status::Idle,
            job: None,
            pending: None,
            ignored: Vec::new(),
        }
    }
}
impl Checker {
    pub fn load() -> Self {
        let mut value = Self::default();
        value.ignored = prefs_path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|s| serde_json::from_slice::<Preferences>(&s).ok())
            .map(|p| p.ignored_versions)
            .unwrap_or_default();
        value
    }
    pub fn begin(&mut self, manual: bool) -> bool {
        self.begin_with(manual, check)
    }
    pub fn begin_with(
        &mut self,
        manual: bool,
        fetch: impl FnOnce() -> Result<String> + Send + 'static,
    ) -> bool {
        if self.job.is_some() {
            return false;
        }
        self.pending = None;
        if !manual {
            self.last_auto = Instant::now();
        }
        let (tx, receiver) = mpsc::channel();
        self.job = Some(CheckJob {
            started: Instant::now(),
            manual,
            receiver,
        });
        self.status = Status::Checking;
        std::thread::spawn(move || {
            let result = fetch().map_err(|e| e.to_string());
            let _ = tx.send((Instant::now(), result));
        });
        true
    }
    pub fn poll(&mut self, now: Instant) -> bool {
        let Some(job) = &self.job else { return false };
        let manual = job.manual;
        let result = match job.receiver.try_recv() {
            Ok((at, result)) => Some(if at.duration_since(job.started) <= CHECK_TIMEOUT {
                result
            } else {
                Err("请求超时".into())
            }),
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("请求失败".into())),
            Err(_) if now.duration_since(job.started) >= CHECK_TIMEOUT => {
                Some(Err("请求超时".into()))
            }
            Err(_) => None,
        };
        let Some(result) = result else { return false };
        self.job = None;
        self.status = match result {
            Ok(version) => match (
                Version::parse(&version),
                Version::parse(env!("CARGO_PKG_VERSION")),
            ) {
                (Ok(remote), Ok(current)) if remote > current => {
                    if manual || !self.ignored.contains(&remote.key()) {
                        self.pending = Some((version, manual));
                    }
                    Status::Idle
                }
                (Ok(_), Ok(_)) => Status::Latest,
                _ => Status::Failed,
            },
            Err(_) => Status::Failed,
        };
        if !manual {
            self.status = Status::Idle;
        }
        true
    }
    pub fn auto_due(&self, now: Instant) -> bool {
        now.duration_since(self.last_auto) >= INTERVAL && self.job.is_none()
    }
    pub fn ignore(&mut self, version: &str) -> Result<()> {
        let key = Version::parse(version)?.key();
        if self.ignored.contains(&key) {
            return Ok(());
        }
        let mut saved = self.ignored.clone();
        saved.push(key);
        let path = prefs_path().context("找不到设置目录")?;
        self.persist_ignored(saved, &path)
    }
    fn persist_ignored(&mut self, saved: Vec<String>, path: &Path) -> Result<()> {
        crate::win::write_atomic(
            &path,
            &serde_json::to_vec_pretty(&Preferences {
                ignored_versions: saved.clone(),
            })?,
        )?;
        self.ignored = saved;
        Ok(())
    }
}
#[derive(Default, Serialize, Deserialize)]
struct Preferences {
    #[serde(default)]
    ignored_versions: Vec<String>,
}
fn prefs_path() -> Option<PathBuf> {
    std::env::var_os("CODEX_SWITCH_UI_FILE")
        .map(PathBuf::from)
        .and_then(|p| p.parent().map(|p| p.join("update.json")))
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(|p| PathBuf::from(p).join("CodexSwitch/update.json"))
        })
}

pub fn file_version(path: &Path) -> Result<Version> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::*;
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let size = GetFileVersionInfoSizeW(name.as_ptr(), std::ptr::null_mut());
        ensure!(size > 0 && size < 1024 * 1024, "下载文件没有有效版本信息");
        let mut bytes = vec![0u8; size as usize];
        ensure!(
            GetFileVersionInfoW(name.as_ptr(), 0, size, bytes.as_mut_ptr() as _) != 0,
            "读取文件版本失败"
        );
        let mut ptr = std::ptr::null_mut();
        let mut len = 0;
        ensure!(
            VerQueryValueW(bytes.as_ptr() as _, [92u16, 0].as_ptr(), &mut ptr, &mut len) != 0
                && len as usize >= std::mem::size_of::<VS_FIXEDFILEINFO>(),
            "下载文件版本无效"
        );
        let value = std::ptr::read_unaligned(ptr as *const VS_FIXEDFILEINFO);
        ensure!(value.dwSignature == 0xfeef04bd, "下载文件版本无效");
        Ok(Version([
            (value.dwFileVersionMS >> 16) as u16,
            value.dwFileVersionMS as u16,
            (value.dwFileVersionLS >> 16) as u16,
            value.dwFileVersionLS as u16,
        ]))
    }
}

#[derive(Serialize, Deserialize)]
pub struct Plan {
    pub target: PathBuf,
    pub staged: PathBuf,
    pub parent: u32,
    pub version: String,
}
pub fn stage(version: &str) -> Result<Plan> {
    let target = std::env::current_exe()?;
    ensure!(
        !crate::codex::is_bridge(&target),
        "后台桥接进程不能执行更新"
    );
    let staged = target.with_file_name(format!(
        ".codex-switch-update-{}-{}.exe",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .context("原程序目录不可写，无法保存更新")?;
        let path = format!(
            "/codex-switch/Codex%E5%88%87%E5%8F%B7.exe?v={}",
            Version::parse(version)?.key()
        );
        http::get(&path, Duration::from_secs(60), 32 * 1024 * 1024, &mut file)?;
        file.sync_all()?;
        drop(file);
        ensure!(
            file_version(&staged)? == Version::parse(version)?,
            "下载文件的版本与服务器版本不一致"
        );
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&staged);
        return Err(error);
    }
    Ok(Plan {
        target,
        staged,
        parent: std::process::id(),
        version: version.into(),
    })
}

pub fn launch_helper(plan: Plan) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let folder = std::env::temp_dir().join(format!(
        "CodexSwitch-update-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    let result = (|| -> Result<()> {
        std::fs::create_dir(&folder)?;
        let helper = folder.join("updater.exe");
        std::fs::copy(std::env::current_exe()?, &helper)?;
        let path = folder.join("plan.json");
        std::fs::write(&path, serde_json::to_vec(&plan)?)?;
        let mut child = std::process::Command::new(&helper)
            .arg("--apply-update")
            .arg(&path)
            .creation_flags(crate::win::CREATE_NO_WINDOW)
            .spawn()
            .context("无法启动更新程序")?;
        let ready = folder.join("ready");
        let deadline = Instant::now() + Duration::from_secs(3);
        while !ready.exists() {
            if child.try_wait()?.is_some() {
                bail!("更新程序未就绪");
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("更新程序准备超时");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&plan.staged);
        for name in ["ready", "plan.json", "updater.exe"] {
            let _ = std::fs::remove_file(folder.join(name));
        }
        let _ = std::fs::remove_dir(folder);
    }
    result
}

/// Replace same-directory staging with rollback; launch is injected for isolated tests.
pub fn install(plan: &Plan, launch: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    ensure!(
        plan.target.is_absolute()
            && plan.staged.is_absolute()
            && plan.target.parent() == plan.staged.parent(),
        "更新路径无效"
    );
    ensure!(
        plan.staged
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with(".codex-switch-update-") && s.ends_with(".exe")),
        "更新文件无效"
    );
    ensure!(
        file_version(&plan.staged)? == Version::parse(&plan.version)?,
        "更新文件版本不匹配"
    );
    let backup = plan.staged.with_extension("backup");
    ensure!(!backup.exists(), "更新备份已存在");
    std::fs::rename(&plan.target, &backup).context("无法替换原程序，原程序仍保留")?;
    if let Err(error) = std::fs::rename(&plan.staged, &plan.target) {
        let _ = std::fs::rename(&backup, &plan.target);
        return Err(error.into());
    }
    if let Err(error) = launch(&plan.target) {
        let _ = std::fs::rename(&plan.target, &plan.staged);
        let _ = std::fs::rename(&backup, &plan.target);
        return Err(error.context("启动新版失败，已尝试恢复旧版"));
    }
    let _ = std::fs::remove_file(backup);
    Ok(())
}

pub fn apply(path: &Path) -> Result<()> {
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::System::Threading::*;
    let plan: Plan = serde_json::from_slice(&std::fs::read(path)?)?;
    let parent = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, plan.parent) };
    ensure!(!parent.is_null(), "更新父进程无效");
    // The original UI stays alive until this acknowledgement exists. Once opened,
    // the handle refers to that process even if its PID is later reused.
    if let Err(error) = std::fs::write(path.with_file_name("ready"), b"ready") {
        unsafe {
            CloseHandle(parent);
        }
        return Err(error.into());
    }
    let wait = unsafe { WaitForSingleObject(parent, 30000) };
    unsafe {
        CloseHandle(parent);
    }
    ensure!(wait == WAIT_OBJECT_0, "旧程序尚未退出，已取消替换");
    let result = install(&plan, |target| {
        std::process::Command::new(target)
            .arg("--updated-from")
            .arg(path)
            .spawn()
            .context("无法启动新版")?;
        Ok(())
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&plan.staged);
    }
    result
}
pub fn cleanup(path: PathBuf) {
    // Only our known temp files; never delete a caller-supplied tree recursively.
    let Some(dir) = path.parent() else { return };
    if dir.parent() != Some(std::env::temp_dir().as_path())
        || path.file_name().and_then(|s| s.to_str()) != Some("plan.json")
        || !dir
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("CodexSwitch-update-"))
    {
        return;
    }
    for _ in 0..20 {
        if std::fs::remove_file(dir.join("updater.exe")).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(dir.join("ready"));
    let _ = std::fs::remove_dir(dir);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn completed(checker: &mut Checker, manual: bool, value: Result<String, String>) {
        let (tx, receiver) = mpsc::channel();
        let started = Instant::now();
        checker.job = Some(CheckJob {
            started,
            manual,
            receiver,
        });
        checker.status = Status::Checking;
        tx.send((started, value)).unwrap();
        assert!(checker.poll(Instant::now()));
    }
    #[test]
    fn numeric_versions_metadata_and_exact_ignore_scope() {
        assert!(Version::parse("0.4.10").unwrap() > Version::parse("0.4.9").unwrap());
        assert_eq!(
            Version::parse("v1.2.3").unwrap(),
            Version::parse("1.2.3.0").unwrap()
        );
        for bad in ["", "1", "1..2", "1.2.rc1", "1.2.3.4.5", "1.65536", "1.-1"] {
            assert!(Version::parse(bad).is_err());
        }
        assert_eq!(
            parse_metadata(br#"{"version":"1.2.3","downloadCount":123,"future":true}"#).unwrap(),
            "1.2.3"
        );
        for bad in [
            br#"{"version":123}"#.as_slice(),
            br#"{"downloadCount":1}"#,
            br#"{"version":"n/a"}"#,
        ] {
            assert!(parse_metadata(bad).is_err());
        }
        let mut checker = Checker::default();
        checker
            .ignored
            .push(Version::parse("65000.0.0").unwrap().key());
        completed(&mut checker, false, Ok("65000.0.0".into()));
        assert!(checker.pending.is_none());
        assert_eq!(checker.status, Status::Idle);
        completed(&mut checker, false, Ok("65000.0.1".into()));
        assert_eq!(checker.pending.take().unwrap().0, "65000.0.1");
        completed(&mut checker, true, Ok("65000.0.0".into()));
        assert!(
            checker.pending.take().unwrap().1,
            "manual checks may explicitly update ignored releases"
        );
    }
    #[test]
    fn failure_and_late_results_never_block_or_trigger_a_retry() {
        let mut checker = Checker::default();
        let original_auto = checker.last_auto;
        let (release, wait) = mpsc::channel();
        assert!(checker.begin_with(true, move || {
            let _ = wait.recv();
            Ok("65000.0.0".into())
        }));
        assert!(!checker.begin_with(true, || panic!("duplicate network request")));
        assert_eq!(
            checker.last_auto, original_auto,
            "manual checks do not move the 24-hour clock"
        );
        let deadline = checker.job.as_ref().unwrap().started + CHECK_TIMEOUT;
        assert!(checker.poll(deadline));
        assert_eq!(checker.status, Status::Failed);
        assert!(checker.job.is_none());
        release.send(()).unwrap();
        assert!(!checker.poll(deadline + Duration::from_secs(1)));
        assert!(checker.pending.is_none());
        completed(&mut checker, false, Err("domain offline".into()));
        assert_eq!(checker.status, Status::Idle);
        assert!(checker.pending.is_none());
        assert!(!checker.auto_due(checker.last_auto + INTERVAL - Duration::from_millis(1)));
        assert!(checker.auto_due(checker.last_auto + INTERVAL));
        completed(&mut checker, true, Ok(env!("CARGO_PKG_VERSION").into()));
        assert_eq!(checker.status, Status::Latest);
        assert!(checker.pending.is_none());
        completed(&mut checker, true, Ok("0.0.0".into()));
        assert_eq!(checker.status, Status::Latest);
    }
    #[test]
    fn late_success_is_discarded_and_ignored_versions_persist_in_isolated_file() {
        let mut checker = Checker::default();
        let (tx, receiver) = mpsc::channel();
        let started = Instant::now();
        checker.job = Some(CheckJob {
            started,
            manual: true,
            receiver,
        });
        tx.send((
            started + CHECK_TIMEOUT + Duration::from_millis(1),
            Ok("65000.0.0".into()),
        ))
        .unwrap();
        assert!(checker.poll(started + CHECK_TIMEOUT));
        assert_eq!(checker.status, Status::Failed);
        assert!(checker.pending.is_none());
        let home = std::env::temp_dir().join(format!(
            "codex-update-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&home).unwrap();
        let path = home.join("update.json");
        checker
            .persist_ignored(vec!["65000.0.0.0".into()], &path)
            .unwrap();
        let saved: Preferences = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.ignored_versions, checker.ignored);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(home).unwrap();
    }
    #[test]
    fn executable_install_and_rollback_use_temp_copies_without_launching_any_program() {
        let home = std::env::temp_dir().join(format!(
            "codex-install-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&home).unwrap();
        let target = home.join("Codex切号.exe");
        let staged = home.join(".codex-switch-update-test.exe");
        let source = std::env::current_exe().unwrap();
        let version = file_version(&source).unwrap().key();
        let plan = Plan {
            target: target.clone(),
            staged: staged.clone(),
            version,
            parent: 0,
        };
        std::fs::write(&target, b"old synthetic executable marker").unwrap();
        std::fs::copy(&source, &staged).unwrap();
        assert!(install(&plan, |_| bail!("synthetic launch failure")).is_err());
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"old synthetic executable marker"
        );
        assert!(staged.exists());
        install(&plan, |path| {
            assert_eq!(path, target);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            file_version(&target).unwrap(),
            file_version(&source).unwrap()
        );
        assert!(!staged.exists());
        assert!(!staged.with_extension("backup").exists());
        std::fs::write(&staged, b"<html>download error</html>").unwrap();
        assert!(install(&plan, |_| panic!("invalid EXE must not launch")).is_err());
        assert!(file_version(&target).is_ok());
        std::fs::remove_file(staged).unwrap();
        std::fs::remove_file(target).unwrap();
        std::fs::remove_dir(home).unwrap();
    }
}
