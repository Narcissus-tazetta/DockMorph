//! gpui の prompt_for_paths は開く場所を指定できず、macOS が覚えている前回の場所で開く。
//! アプリは /Applications にあることが大半なので、アプリ選択だけ自前の NSOpenPanel で開く場所を決める。

use block2::RcBlock;
use futures::channel::oneshot;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSModalResponse, NSModalResponseOK, NSOpenPanel};
use objc2_foundation::{NSString, NSURL};
use std::cell::Cell;
use std::path::{Path, PathBuf};

/// ファイルを 1 つ選ぶパネルを `directory` で開く。メインスレッドから呼ぶこと
pub fn pick_file(prompt: &str, directory: &Path) -> oneshot::Receiver<anyhow::Result<Option<Vec<PathBuf>>>> {
    let (tx, rx) = oneshot::channel();
    let Some(mtm) = MainThreadMarker::new() else {
        let _ = tx.send(Err(anyhow::anyhow!("ファイル選択パネルはメインスレッドでしか開けません")));
        return rx;
    };
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    panel.setResolvesAliases(false);
    panel.setPrompt(Some(&NSString::from_str(prompt)));
    panel.setDirectoryURL(Some(&NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&directory.to_string_lossy()), true)));

    let tx = Cell::new(Some(tx));
    let held = panel.clone();
    // ブロックがパネルを保持するので、完了までパネルは解放されない
    let done = RcBlock::new(move |response: NSModalResponse| {
        let picked = (response == NSModalResponseOK).then(|| {
            held.URLs().iter().filter_map(|url| url.path()).map(|p| PathBuf::from(p.to_string())).collect::<Vec<_>>()
        });
        if let Some(tx) = tx.take() {
            // 受け手(ウィンドウ)が先に閉じていたら、届け先がないだけ
            let _ = tx.send(Ok(picked));
        }
    });
    panel.beginWithCompletionHandler(&done);
    rx
}
