use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

// Antigravity は再フォーカス時にも setIcon を呼び、Dock のプラグインより優先される。
// 呼び出しを確認していない他の Electron アプリには介入しない。
pub const BUNDLE_ID: &str = "com.google.antigravity";

#[derive(Clone, PartialEq)]
pub struct Process {
    pub pid: i32,
    pub started: (u64, u64),
    pub bundle: PathBuf,
    pub registry: PathBuf,
}

pub fn update(process: &Process, restore: bool) -> Result<()> {
    let helper = std::env::current_exe()?.parent().context("DockMorph の実行場所が分かりません")?.join("../Resources/DMIconGuard");
    let output = Command::new(&helper)
        .arg(if restore { "restore" } else { "apply" })
        .arg(process.pid.to_string())
        .arg(process.started.0.to_string())
        .arg(process.started.1.to_string())
        .arg(&process.bundle)
        .arg(&process.registry)
        .output()
        .with_context(|| format!("起動中のアイコン制御を開始できません: {}", helper.display()))?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}
