//! 定位并控制 Codex Desktop。
//!
//! Codex Desktop 通常是 Store（MSIX）应用，包名 `OpenAI.Codex`，可执行文件可能叫
//! `ChatGPT.exe`。这里沿用原作者的做法：从 AppxManifest.xml 拿可执行文件与 AUMID，
//! 关闭用正常 WM_CLOSE，打开用 `shell:AppsFolder\<AUMID>`。

use crate::win;
use anyhow::{bail, Context, Result};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

const DISCOVER_SCRIPT: &str = r#"
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$p = Get-AppxPackage -Name OpenAI.Codex | Select-Object -First 1
if (-not $p) { exit 3 }
[xml]$m = Get-Content -LiteralPath (Join-Path $p.InstallLocation 'AppxManifest.xml')
$a = @($m.Package.Applications.Application)[0]
@{ executable = (Join-Path $p.InstallLocation $a.Executable); appId = ($p.PackageFamilyName + '!' + $a.Id) } | ConvertTo-Json -Compress
"#;

#[derive(Debug, Clone)]
pub struct Installation {
    pub executable: PathBuf,
    pub aumid: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Running {
    pub pid: u32,
    pub path: PathBuf,
}

pub struct Desktop {
    pub installation: Installation,
}

impl Desktop {
    pub fn discover() -> Result<Self> {
        // Isolated desktop fixture/custom installation; never guesses a process by name.
        if let Some(executable) = std::env::var_os("CODEX_SWITCH_DESKTOP_EXE") {
            let executable = PathBuf::from(executable);
            if !executable.is_file() {
                bail!("指定的 Codex Desktop 不存在");
            }
            return Ok(Self {
                installation: Installation {
                    executable,
                    aumid: None,
                },
            });
        }
        if let Some(installation) = discover_store_package()? {
            return Ok(Self { installation });
        }
        for candidate in fallback_candidates() {
            if candidate.exists() {
                return Ok(Self {
                    installation: Installation {
                        executable: candidate,
                        aumid: None,
                    },
                });
            }
        }
        bail!("没有找到 Codex Desktop。请先安装 Codex Desktop 再切换账号。")
    }

    pub fn running(&self) -> Vec<Running> {
        let target = &self.installation.executable;
        win::processes()
            .into_iter()
            .filter_map(|process| {
                let path = process.path?;
                if same_path(&path, target) {
                    Some(Running {
                        pid: process.pid,
                        path,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// 先请求正常退出；如果它只是缩到托盘（进程还在），再强制结束后台进程。
    /// `force = false` 时行为退化为“只请求、不强制”，便于只读场景。
    pub fn close(&self, force: bool) -> Result<()> {
        let running = self.running();
        if running.is_empty() {
            return Ok(());
        }
        let pids: Vec<u32> = running.iter().map(|process| process.pid).collect();
        let windows: Vec<usize> = win::visible_windows()
            .into_iter()
            .filter(|(pid, _)| pids.contains(pid))
            .map(|(_, window)| window)
            .collect();

        if windows.is_empty() {
            if !force {
                bail!("Codex Desktop 正在运行但没有可关闭的窗口。请手动退出后重试（账号未改动）。");
            }
        } else {
            for window in windows {
                win::post_close(window);
            }
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(8) {
                std::thread::sleep(Duration::from_millis(200));
                if self.running().is_empty() {
                    return Ok(());
                }
            }
            if !force {
                bail!("Codex Desktop 没有在 8 秒内退出（它可能只是缩到了托盘）。账号未改动。");
            }
        }

        println!("  Codex Desktop 只是缩到了托盘，正在结束后台进程…");
        for pid in &pids {
            win::terminate_tree(*pid)?;
        }

        let forced = Instant::now();
        while forced.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(200));
            if self.running().is_empty() {
                return Ok(());
            }
        }
        bail!("无法结束 Codex Desktop。请从托盘手动退出后重试（账号未改动）。");
    }

    pub fn reopen_connected(
        &self,
        wrapper: &std::path::Path,
        home: &std::path::Path,
    ) -> Result<()> {
        // Explorer/Store activation can inherit stale environment variables. Launch the
        // actual installed executable with an explicit environment on this first connection.
        win::launch_desktop(&self.installation.executable, wrapper, home)?;
        self.wait_running()
    }

    fn wait_running(&self) -> Result<()> {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(15) {
            if !self.running().is_empty() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        bail!("Codex Desktop 15 秒内没有启动，账号未改动")
    }
}

fn fallback_candidates() -> Vec<PathBuf> {
    let mut result = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let base = PathBuf::from(local);
        result.push(base.join("Programs").join("Codex").join("Codex.exe"));
        result.push(base.join("OpenAI").join("Codex").join("Codex.exe"));
    }
    result
}

fn discover_store_package() -> Result<Option<Installation>> {
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", DISCOVER_SCRIPT])
        .creation_flags(crate::win::CREATE_NO_WINDOW)
        .output()
        .context("无法调用 powershell 查询 Store 版 Codex")?;

    if !output.status.success() {
        return Ok(None);
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }

    let value: serde_json::Value =
        serde_json::from_str(text).context("解析 Codex Desktop 安装信息失败")?;
    let executable = value
        .get("executable")
        .and_then(|value| value.as_str())
        .context("安装信息里缺少可执行文件路径")?;
    let app_id = value
        .get("appId")
        .and_then(|value| value.as_str())
        .map(str::to_owned);

    let executable = PathBuf::from(executable);
    if !executable.exists() {
        return Ok(None);
    }
    Ok(Some(Installation {
        executable,
        aumid: app_id,
    }))
}

fn same_path(left: &std::path::Path, right: &std::path::Path) -> bool {
    left.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
}
