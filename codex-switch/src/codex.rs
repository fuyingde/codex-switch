//! 找到 `codex.exe`（app-server 和额度查询都依赖它）。

use std::path::PathBuf;

pub fn locate() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("CODEX_CLI_PATH") {
        let path = PathBuf::from(explicit);
        if path.is_file() && !is_bridge(&path) {
            return Some(path);
        }
    }
    desktop_cli()
}

/// Match the Desktop runtime before considering an independently installed CLI on PATH.
pub fn desktop_cli() -> Option<PathBuf> {
    from_local_app_data().or_else(from_path)
}

fn from_path() -> Option<PathBuf> {
    let value = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&value) {
        for name in ["codex.exe", "codex"] {
            let candidate = directory.join(name);
            if candidate.is_file() && !is_bridge(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

pub fn is_bridge(path: &std::path::Path) -> bool {
    path.file_stem().is_some_and(|name| {
        name.to_string_lossy()
            .eq_ignore_ascii_case("codex-switch-bridge")
    })
}

/// 桌面版会把 CLI 放在 `%LOCALAPPDATA%\OpenAI\Codex\bin\<版本>\codex.exe`，取最新的一个。
fn from_local_app_data() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    let bin = PathBuf::from(local)
        .join("OpenAI")
        .join("Codex")
        .join("bin");
    let entries = std::fs::read_dir(&bin).ok()?;
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join("codex.exe"))
        .filter(|path| path.is_file())
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    candidates.into_iter().next().map(|(_, path)| path)
}

pub fn describe_search() -> String {
    let path_state = match from_path() {
        Some(path) => format!("PATH 里找到了 {}", path.display()),
        None => "PATH 里没有 codex.exe".to_string(),
    };
    let local_state = match from_local_app_data() {
        Some(path) => format!("LOCALAPPDATA 里找到了 {}", path.display()),
        None => "LOCALAPPDATA\\OpenAI\\Codex\\bin 下没有找到".to_string(),
    };
    format!("{path_state}；{local_state}")
}
