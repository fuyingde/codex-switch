//! 把图标资源写进 EXE（文件图标 + 任务栏/托盘图标）。
//!
//! 用 Windows SDK 自带的 rc.exe 在构建时编译 .rc；找不到 rc.exe 或图标文件就跳过，
//! 编译仍然成功，只是 EXE 没有自定义图标。
//!
//! 注意两个坑：
//! 1. `canonicalize()` 会返回 `\\?\H:\...` 扩展路径，rc.exe 不认，必须去掉前缀；
//! 2. .rc 里的路径要写成正斜杠，否则 `\a` 会被当成转义序列吃掉。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=build.rs");

    let Some(icon) = icon_path() else {
        return;
    };
    let Some(rc) = find_rc() else {
        return;
    };

    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };
    let out = PathBuf::from(out_dir);
    let script = out.join("icon.rc");
    let resource = out.join("icon.res");

    let icon_text = icon.display().to_string().replace('\\', "/");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.2.0".into());
    let numeric_version = format!(
        "{},0",
        version.split('.').take(3).collect::<Vec<_>>().join(",")
    );
    let content = format!(
        r#"1 ICON "{icon_text}"

1 VERSIONINFO
FILEVERSION {numeric_version}
PRODUCTVERSION {numeric_version}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "080404B0"
    BEGIN
      VALUE "FileDescription", "Codex切号器"
      VALUE "ProductName", "Codex切号器"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "LegalCopyright", "MIT"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x804, 1200
  END
END
"#
    );
    // rc.exe 默认按系统代码页读 .rc，中文会变乱码；写成带 BOM 的 UTF-16LE 即可。
    let mut bytes = Vec::with_capacity(content.len() * 2 + 2);
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    for unit in content.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    if std::fs::write(&script, bytes).is_err() {
        return;
    }

    let status = Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&resource)
        .arg(&script)
        .status();

    if matches!(status, Ok(value) if value.success()) && resource.exists() {
        println!("cargo:rustc-link-arg={}", resource.display());
    }
}

fn icon_path() -> Option<PathBuf> {
    let path = Path::new("assets/app.ico");
    if !path.exists() {
        return None;
    }
    let absolute = std::env::current_dir().ok()?.join(path);
    let clean = absolute.canonicalize().unwrap_or(absolute);
    let text = clean.display().to_string();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text).to_string();
    Some(PathBuf::from(text))
}

fn find_rc() -> Option<PathBuf> {
    let base = Path::new("C:/Program Files (x86)/Windows Kits/10/bin");
    let entries = std::fs::read_dir(base).ok()?;
    let mut best: Option<(String, PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let candidate = entry.path().join("x64").join("rc.exe");
        if candidate.exists() && best.as_ref().map(|(best, _)| &name > best).unwrap_or(true) {
            best = Some((name, candidate));
        }
    }
    best.map(|(_, path)| path)
}
