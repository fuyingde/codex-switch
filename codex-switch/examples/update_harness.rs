//! Process-level update fixture: no UI, network, accounts or real installation.
#![allow(dead_code)]
#[path = "../src/update.rs"]
mod update;
#[path = "../src/win.rs"]
mod win;
mod codex {
    pub fn is_bridge(path: &std::path::Path) -> bool {
        path.file_stem().is_some_and(|s| {
            s.to_string_lossy()
                .eq_ignore_ascii_case("codex-switch-bridge")
        })
    }
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
fn run() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.first().and_then(|s| s.to_str()) {
        Some("--apply-update") => update::apply(std::path::Path::new(&args[1])),
        Some("--updated-from") => {
            let path = std::path::PathBuf::from(&args[1]);
            let plan: update::Plan = serde_json::from_slice(&std::fs::read(&path)?)?;
            let marker = plan.target.with_extension("launched.json");
            std::fs::write(
                marker,
                serde_json::to_vec(
                    &serde_json::json!({"version":update::file_version(&std::env::current_exe()?)?.key(),"target":plan.target,"helperFolder":path.parent()}),
                )?,
            )?;
            update::cleanup(path);
            Ok(())
        }
        Some("fixture") => {
            let target = std::path::PathBuf::from(&args[1]);
            let staged = std::path::PathBuf::from(&args[2]);
            let version = update::file_version(&staged)?.key();
            update::launch_helper(update::Plan {
                target,
                staged,
                version,
                parent: std::process::id(),
            })
        }
        _ => anyhow::bail!("fixture arguments required"),
    }
}
