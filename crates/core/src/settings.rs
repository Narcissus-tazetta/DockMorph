use crate::lang::Language;
use crate::DockMorph;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// アプリ全体の設定。未知のキーは無視し、無いキーは既定値になる
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// 画面で選ばれるまでは None で、macOS の言語に合わせる
    pub language: Option<Language>,
    /// 30 秒ごとの自動同期(アップデートで外れたアイコンの付け直しなど)
    pub auto_sync: bool,
    /// 起動中の Antigravity が元のアイコンに戻すのを抑止する
    pub keep_running_icon: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { language: None, auto_sync: true, keep_running_icon: true }
    }
}

impl DockMorph {
    fn settings_file(&self) -> PathBuf {
        self.home.join("settings.json")
    }

    pub fn settings(&self) -> Result<Settings> {
        let file = self.settings_file();
        match fs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| crate::l!("設定ファイルが壊れています: {}", "The settings file is corrupted: {}", file.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e).with_context(|| crate::l!("設定ファイルを読めません: {}", "Cannot read the settings file: {}", file.display())),
        }
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        fs::create_dir_all(&self.home)?;
        let file = self.settings_file();
        let tmp = file.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(settings)?).with_context(|| crate::l!("設定を保存できません: {}", "Cannot save the settings: {}", tmp.display()))?;
        fs::rename(&tmp, &file).with_context(|| crate::l!("設定を保存できません: {}", "Cannot save the settings: {}", file.display()))
    }
}
