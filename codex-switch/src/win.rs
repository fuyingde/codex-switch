//! 需要的 Windows 原语：原子替换、进程枚举、窗口枚举与关闭。
//!
//! 注意：windows-sys 0.59 起 `HWND` / `HANDLE` 是 `*mut c_void` 而不是 `isize`。
//! 这里对外只暴露 `usize`，避免把裸指针类型带进业务代码。

use anyhow::{bail, Context, Result};
use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
};

#[derive(Debug, Clone)]
pub struct ProcessEntry {
    pub pid: u32,
    pub path: Option<PathBuf>,
}

fn wide(value: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Bridge capabilities are local secrets: restrict their directory/files to this user and SYSTEM.
pub fn protect_private_path(path: &Path) -> Result<()> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, SetFileSecurityW, TokenUser, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            bail!("无法获取当前用户的文件访问权限");
        }
        let mut needed = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        );
        CloseHandle(token);
        if ok == 0 {
            bail!("无法读取当前用户 SID");
        }
        let user = &*(buffer.as_ptr().cast::<TOKEN_USER>());
        let mut sid = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
            bail!("无法转换当前用户 SID");
        }
        let mut length = 0;
        while *sid.add(length) != 0 {
            length += 1;
        }
        let sid_text = String::from_utf16_lossy(std::slice::from_raw_parts(sid, length));
        LocalFree(sid.cast());
        let inherit = if path.is_dir() { "OICI" } else { "" };
        let sddl = wide(Path::new(&format!(
            "D:P(A;{inherit};FA;;;SY)(A;{inherit};FA;;;{sid_text})"
        )));
        let mut descriptor = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            bail!("无法创建私有文件访问权限");
        }
        let ok = SetFileSecurityW(
            wide(path).as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        );
        LocalFree(descriptor);
        if ok == 0 {
            bail!("无法保护本地桥接文件：{}", std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// 先写临时文件，再用 MoveFileEx 原子换名，避免中途留下半个 auth.json。
pub fn write_atomic(destination: &Path, bytes: &[u8]) -> Result<()> {
    let parent = destination.parent().context("目标路径没有父目录")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("创建目录失败：{}", parent.display()))?;

    let temporary = destination.with_file_name(format!(
        "{}.tmp{}",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("auth.json"),
        std::process::id()
    ));

    std::fs::write(&temporary, bytes)
        .with_context(|| format!("写入临时文件失败：{}", temporary.display()))?;

    match replace_file(&temporary, destination) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(error)
        }
    }
}

pub fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    let source_wide = wide(source);
    let destination_wide = wide(destination);
    let result = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        bail!(
            "原子替换失败：{} → {}（{}）",
            source.display(),
            destination.display(),
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

pub fn processes() -> Vec<ProcessEntry> {
    let mut result = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot as isize == -1 {
            return result;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                result.push(ProcessEntry {
                    pid: entry.th32ProcessID,
                    path: image_path(entry.th32ProcessID),
                });
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    result
}

fn image_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if result == 0 || size == 0 {
            return None;
        }
        Some(PathBuf::from(String::from_utf16_lossy(
            &buffer[..size as usize],
        )))
    }
}

/// 顶层可见窗口 → (进程 ID, 窗口句柄)。句柄以 `usize` 形式对外暴露。
pub fn visible_windows() -> Vec<(u32, usize)> {
    let mut result: Vec<(u32, usize)> = Vec::new();
    unsafe {
        EnumWindows(
            Some(collect_window),
            &mut result as *mut Vec<(u32, usize)> as isize,
        );
    }
    result
}

unsafe extern "system" fn collect_window(hwnd: *mut c_void, lparam: isize) -> i32 {
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    let result = &mut *(lparam as *mut Vec<(u32, usize)>);
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid != 0 {
        result.push((pid, hwnd as usize));
    }
    1
}

pub fn post_close(window: usize) {
    unsafe {
        PostMessageW(window as *mut c_void, WM_CLOSE, 0, 0);
    }
}

/// 当前鼠标位置（屏幕坐标）。
pub fn cursor_pos() -> Option<(i32, i32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    unsafe {
        let mut point = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut point) == 0 {
            None
        } else {
            Some((point.x, point.y))
        }
    }
}

/// 主屏尺寸，用来把窗口水平居中。
pub fn primary_screen_size() -> (i32, i32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

/// 把 Unix 秒转成本地时间 (年, 月, 日, 时, 分)，时区交给 Windows 处理。
pub fn local_datetime(epoch_seconds: i64) -> Option<(u16, u16, u16, u16, u16)> {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    use windows_sys::Win32::System::Time::FileTimeToSystemTime;

    // FILETIME 是自 1601-01-01 起的 100 纳秒计数。
    let total = (epoch_seconds + 11_644_473_600).checked_mul(10_000_000)?;
    let file_time = FILETIME {
        dwLowDateTime: (total & 0xFFFF_FFFF) as u32,
        dwHighDateTime: ((total >> 32) & 0xFFFF_FFFF) as u32,
    };
    unsafe {
        let mut local = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        if FileTimeToLocalFileTime(&file_time, &mut local) == 0 {
            return None;
        }
        let mut system: SYSTEMTIME = std::mem::zeroed();
        if FileTimeToSystemTime(&local, &mut system) == 0 {
            return None;
        }
        Some((
            system.wYear,
            system.wMonth,
            system.wDay,
            system.wHour,
            system.wMinute,
        ))
    }
}

/// 启动子进程时不创建控制台窗口。
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A long-lived Desktop must not inherit the switcher's pipes or handles. In
/// particular, inherited capture pipes keep CLI callers waiting after setup exits.
pub fn launch_desktop(executable: &Path, wrapper: &Path, home: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION, STARTUPINFOW,
    };
    let mut vars: Vec<_> = std::env::vars_os()
        .filter(|(name, _)| {
            let name = name.to_string_lossy();
            !name.eq_ignore_ascii_case("CODEX_CLI_PATH") && !name.eq_ignore_ascii_case("CODEX_HOME")
        })
        .collect();
    vars.push(("CODEX_CLI_PATH".into(), wrapper.as_os_str().to_owned()));
    vars.push(("CODEX_HOME".into(), home.as_os_str().to_owned()));
    vars.sort_by_key(|(name, _)| name.to_string_lossy().to_uppercase());
    let mut environment = Vec::<u16>::new();
    for (name, value) in vars {
        environment.extend(name.encode_wide());
        environment.push(b'=' as u16);
        environment.extend(value.encode_wide());
        environment.push(0);
    }
    environment.push(0);
    let mut command = wide(Path::new(&format!("\"{}\"", executable.display())));
    unsafe {
        let mut startup: STARTUPINFOW = std::mem::zeroed();
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut process: PROCESS_INFORMATION = std::mem::zeroed();
        if CreateProcessW(
            wide(executable).as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            std::ptr::null(),
            &startup,
            &mut process,
        ) == 0
        {
            bail!(
                "无法启动 Codex Desktop：{}",
                std::io::Error::last_os_error()
            );
        }
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    Ok(())
}

pub struct ConnectionLock(*mut c_void);
impl Drop for ConnectionLock {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Threading::ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

/// Cross-process guard: two switcher instances must not restart Desktop concurrently.
pub fn connection_lock(root: &Path) -> Result<ConnectionLock> {
    use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
    let hash = root
        .to_string_lossy()
        .to_lowercase()
        .bytes()
        .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
    let name = wide(Path::new(&format!(
        "Local\\CodexSwitchConnection-{hash:016x}"
    )));
    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if handle.is_null() {
            bail!("无法创建无感连接锁");
        }
        let result = WaitForSingleObject(handle, 0);
        if result != 0 && result != 0x80 {
            CloseHandle(handle);
            bail!("另一个切号器正在连接 Codex，请稍候重试");
        }
        Ok(ConnectionLock(handle))
    }
}

/// 正常退出失败时，强制结束整个进程树（`taskkill /T /F`）。
pub fn terminate_tree(pid: u32) -> Result<()> {
    let output = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("无法调用 taskkill")?;
    // 进程可能在这一瞬间已经退出，失败不算错误。
    let _ = output;
    Ok(())
}
