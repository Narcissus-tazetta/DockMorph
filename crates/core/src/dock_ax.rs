//! Dock のタイルのメニュー(オプション → Dock から削除)を AX で押して、ピン留めを外す。
//! defaults import + killall Dock と違って Dock を再起動しないので、起動中のほかの動くタイルが分かれない(実測)。
//! 呼び出し元のプロセスにアクセシビリティの許可が要る。

use anyhow::{anyhow, bail, Context, Result};
use objc2_application_services::{kAXTrustedCheckOptionPrompt, AXError, AXIsProcessTrusted, AXIsProcessTrustedWithOptions, AXUIElement};
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType, CFURL};
use std::collections::HashSet;
use std::path::Path;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

const DOCK_APP: &str = "/System/Library/CoreServices/Dock.app";

pub fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// システムの許可ダイアログを出す(未許可のとき)。この呼び出しで初めてアプリがアクセシビリティの一覧に載る
pub fn request_trust() -> bool {
    let key: &CFString = unsafe { kAXTrustedCheckOptionPrompt };
    let opts = CFDictionary::<CFString, CFBoolean>::from_slices(&[key], &[CFBoolean::new(true)]);
    unsafe { AXIsProcessTrustedWithOptions(Some(opts.as_opaque())) }
}

/// app を指すタイルのピン留めを外す。そのタイルが Dock に無ければ Ok(false)
pub fn unpin(dock_pid: u32, app: &Path) -> Result<bool> {
    if !trusted() {
        bail!("アクセシビリティの許可がありません");
    }
    let dock = unsafe { AXUIElement::new_application(dock_pid as libc::pid_t) };
    unsafe { dock.set_messaging_timeout(2.0) };
    let list = children(&dock).into_iter().find(|e| string(e, "AXRole").as_deref() == Some("AXList")).ok_or_else(|| anyhow!("Dock のタイルの一覧が見つかりません"))?;
    let want = app.canonicalize()?;
    let Some(tile) = children(&list).into_iter().find(|t| tile_path(t).is_some_and(|p| p.canonicalize().ok().as_deref() == Some(want.as_path()))) else {
        return Ok(false);
    };

    let (options, remove, keep) = (dock_strings("OPTIONS")?, dock_strings("REMOVE_FROM_DOCK")?, dock_strings("KEEP_IN_DOCK")?);
    let is = |e: &AXUIElement, set: &HashSet<String>| string(e, "AXTitle").is_some_and(|t| set.contains(&t));
    perform(&tile, "AXShowMenu")?;
    let menu = wait_for(Duration::from_secs(1), || children(&tile).into_iter().find(|e| string(e, "AXRole").as_deref() == Some("AXMenu")))
        .ok_or_else(|| anyhow!("タイルのメニューが開きません"))?;
    let result = (|| {
        let items = children(&menu);
        let submenu = items
            .iter()
            .find(|i| is(i, &options))
            .and_then(|i| children(i).into_iter().next())
            .ok_or_else(|| anyhow!("メニューに「オプション」がありません(項目: {})", titles(&items)))?;
        let items = children(&submenu);
        // 起動中のアプリのタイルには「Dock から削除」が無く、チェックの付いた「Dock に残す」を外すとピン留めが外れる
        let target = items
            .iter()
            .find(|i| is(i, &remove))
            .or_else(|| items.iter().find(|i| is(i, &keep) && string(i, "AXMenuItemMarkChar").is_some_and(|m| !m.is_empty())))
            .ok_or_else(|| anyhow!("メニューに「Dock から削除」がありません(項目: {})", titles(&items)))?;
        perform(target, "AXPress")
    })();
    if result.is_err() {
        // 開いたメニューを残さない。閉じられなくても元のエラーのほうが原因に近いので、そちらを返す
        let _ = perform(&menu, "AXCancel");
    }
    result.map(|()| true)
}

/// Dock のメニューの文言を、Dock 自身の全言語の翻訳から集める。
/// どの言語で表示されているかは呼び出し元のプロセスからは決められない(言語設定を持たない CLI は英語を引く。実測)
fn dock_strings(key: &str) -> Result<HashSet<String>> {
    let mut found = HashSet::new();
    for dir in std::fs::read_dir(Path::new(DOCK_APP).join("Contents/Resources"))? {
        let file = dir?.path().join("DockMenus.strings");
        if !file.exists() {
            continue;
        }
        let table = plist::Value::from_file(&file).with_context(|| format!("Dock の翻訳を読めません: {}", file.display()))?;
        if let Some(v) = table.as_dictionary().and_then(|d| d.get(key)).and_then(|v| v.as_string()) {
            found.insert(v.to_string());
        }
    }
    if found.is_empty() {
        bail!("Dock の翻訳に {key} がありません");
    }
    Ok(found)
}

fn attr(e: &AXUIElement, name: &str) -> Option<CFRetained<CFType>> {
    let mut value: *const CFType = std::ptr::null();
    let err = unsafe { e.copy_attribute_value(&CFString::from_str(name), NonNull::from(&mut value)) };
    if err != AXError::Success {
        return None;
    }
    NonNull::new(value.cast_mut()).map(|v| unsafe { CFRetained::from_raw(v) })
}

fn string(e: &AXUIElement, name: &str) -> Option<String> {
    attr(e, name)?.downcast::<CFString>().ok().map(|s| s.to_string())
}

fn children(e: &AXUIElement) -> Vec<CFRetained<AXUIElement>> {
    let Some(array) = attr(e, "AXChildren").and_then(|v| v.downcast::<CFArray>().ok()) else { return vec![] };
    let array = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(array) };
    (0..array.len()).filter_map(|i| array.get(i)?.downcast::<AXUIElement>().ok()).collect()
}

fn tile_path(tile: &AXUIElement) -> Option<std::path::PathBuf> {
    attr(tile, "AXURL")?.downcast::<CFURL>().ok()?.to_file_path()
}

fn titles(items: &[CFRetained<AXUIElement>]) -> String {
    items.iter().filter_map(|i| string(i, "AXTitle")).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" / ")
}

fn perform(e: &AXUIElement, action: &str) -> Result<()> {
    let err = unsafe { e.perform_action(&CFString::from_str(action)) };
    if err != AXError::Success {
        bail!("{action} が失敗しました (AXError {})", err.0);
    }
    Ok(())
}

fn wait_for<T>(limit: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if start.elapsed() > limit {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
