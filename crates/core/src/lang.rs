use objc2_foundation::NSLocale;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Language {
    English,
    Japanese,
}

static JAPANESE: AtomicBool = AtomicBool::new(false);

fn system_prefers_japanese() -> bool {
    NSLocale::preferredLanguages().firstObject().is_some_and(|first| first.to_string().starts_with("ja"))
}

/// 選ばれていないときの言語。macOS の優先言語が日本語のときだけ日本語、それ以外は英語
pub fn system_default() -> Language {
    if system_prefers_japanese() { Language::Japanese } else { Language::English }
}

/// 選ばれた言語。未選択なら macOS の言語に合わせる
pub fn resolve(chosen: Option<Language>) -> Language {
    chosen.unwrap_or_else(system_default)
}

pub fn set(chosen: Option<Language>) {
    JAPANESE.store(resolve(chosen) == Language::Japanese, Ordering::Relaxed);
}

pub fn is_japanese() -> bool {
    JAPANESE.load(Ordering::Relaxed)
}

/// 文字列リテラルを言語で選ぶ
pub fn tr(ja: &'static str, en: &'static str) -> &'static str {
    if is_japanese() { ja } else { en }
}

/// 引数つきの文言を言語で選ぶ。書式は format! と同じで、選ばれた側だけが評価される
#[macro_export]
macro_rules! l {
    ($ja:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::lang::is_japanese() { format!($ja $(, $arg)*) } else { format!($en $(, $arg)*) }
    };
}
