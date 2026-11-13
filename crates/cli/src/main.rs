use anyhow::{bail, Result};
use dockmorph_core::{DockMorph, Mode, SyncResult, TileState};
use std::path::PathBuf;

const USAGE: &str = "usage:
  dockmorph add [--clone] <App.app> <image>
                                    本物にアイコンを付ける。書き込めなければクローンに切り替える(--clone でクローン固定)
  dockmorph list                    登録とアイコンの場所を表示
  dockmorph sync                    元アプリの更新を検知して付け直す・作り直す
  dockmorph set-image <bundle-id> <image>
                                    登録済みのアプリの画像を差し替える
  dockmorph animate <bundle-id> on <Launcher.app> [--no-pin]
  dockmorph animate <bundle-id> off
                                    Dock で画像を動かす(実験的)。起動用アプリを作って Dock にピン留めする(Dock が再起動する)。
                                    --no-pin ならピン留めせず、起動用アプリから開いたときだけ Dock の右側で動く。
                                    Launcher.app は DockMorph.app/Contents/Resources にある雛形
  dockmorph rejoin <bundle-id>      本物のタイルが別に出ているとき、アプリを再起動して動くタイル 1 つに戻す
  dockmorph upgrade-launchers <Launcher.app>
                                    作成済みの起動用アプリを新しい雛形に入れ替える
  dockmorph refresh-dock            Dock が覚えているアイコンを消して Dock を再起動する(アプリは終了しない)
  dockmorph launch <bundle-id>      起動
  dockmorph remove <bundle-id>      アイコンを戻し、登録を削除";

fn main() -> Result<()> {
    // Rust は SIGPIPE を無視するので、`list | head` で読み手が先に閉じると println! が panic する。
    // ほかの CLI と同じく、黙って終了させる
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dm = DockMorph::new()?;
    match args.first().map(String::as_str) {
        Some("add") => {
            let force_clone = args.iter().any(|a| a == "--clone");
            let paths: Vec<&String> = args[1..].iter().filter(|a| *a != "--clone").collect();
            if paths.len() != 2 {
                bail!("{USAGE}");
            }
            let r = dm.add(&PathBuf::from(paths[0]), &PathBuf::from(paths[1]), force_clone)?;
            if let Some(reason) = &r.fallback_reason {
                eprintln!("warning: 本物に書き込めなかったためクローン方式に切り替えました: {reason}");
            }
            let mode = if r.entry.mode == Mode::InPlace { "inPlace" } else { "clone" };
            println!("added {} [{mode}]", r.entry.bundle_id);
            if r.entry.mode == Mode::Clone {
                println!("clone: {}", dm.clone_path(&r.entry)?.display());
            }
        }
        Some("list") if args.len() == 1 => {
            for e in dm.load_entries()? {
                let mode = if e.mode == Mode::InPlace { "inPlace" } else { "clone" };
                let state = if dm.is_applied(&e) { "applied" } else { "missing" };
                let anim = match dm.tile_state(&e) {
                    None => "",
                    Some(TileState::NotRunning) => "\tanimated",
                    Some(TileState::Joined) => "\tanimated (起動中・タイル 1 つ)",
                    Some(TileState::Split) => "\tanimated (起動中・本物のタイルが別に出ている)",
                };
                println!("{}\t{mode}\t{state}\t{}{anim}", e.bundle_id, dm.target_path(&e)?.display());
            }
        }
        Some("sync") if args.len() == 1 => {
            let mut failed = false;
            for r in dm.sync()? {
                match r {
                    SyncResult::UpToDate(id) => println!("up-to-date {id}"),
                    SyncResult::Recloned(id) => println!("recloned   {id}"),
                    SyncResult::Reapplied(id) => println!("reapplied  {id}"),
                    SyncResult::Relocated(id, path) => println!("relocated  {id} → {}", path.display()),
                    SyncResult::SkippedRunning(id) => println!("skipped (clone running) {id}"),
                    SyncResult::Failed(id, why) => {
                        eprintln!("failed     {id}: {why}");
                        failed = true;
                    }
                }
            }
            if failed {
                std::process::exit(1);
            }
        }
        Some("set-image") if args.len() == 3 => {
            let e = dm.change_image(&args[1], &PathBuf::from(&args[2]))?;
            println!("changed {} → {}", e.bundle_id, e.icon_path.display());
            if dm.dock_stale(std::slice::from_ref(&e)) == [true] {
                println!("note: Dock は前のアイコンを覚えています。refresh-dock で反映されます");
            }
        }
        Some("animate") if (args.len() == 4 || (args.len() == 5 && args[4] == "--no-pin")) && args[2] == "on" => {
            let r = dm.enable_animation(&args[1], &PathBuf::from(&args[3]), args.len() == 4)?;
            if let Some(reason) = &r.fallback_reason {
                eprintln!("warning: {reason}");
            }
            if let Some(a) = &r.entry.animation {
                let mode = if a.direct { "direct" } else { "separate" };
                println!("animated {} → {} [{mode}]{}", r.entry.bundle_id, a.launcher_path.display(), if a.replaced_pin { " (ピン留めを置き換え)" } else { "" });
            }
        }
        Some("animate") if args.len() == 3 && args[2] == "off" => {
            if let Some(w) = dm.disable_animation(&args[1])? {
                eprintln!("warning: {w}");
            }
            println!("stopped {}", args[1]);
        }
        Some("rejoin") if args.len() == 2 => dm.rejoin(&args[1])?,
        Some("upgrade-launchers") if args.len() == 2 => {
            for name in dm.upgrade_launchers(&PathBuf::from(&args[1]))? {
                println!("upgraded {name}");
            }
        }
        Some("refresh-dock") if args.len() == 1 => dm.refresh_dock()?,
        Some("launch") if args.len() == 2 => dm.launch(&args[1])?,
        Some("remove") if args.len() == 2 => {
            if let Some(w) = dm.remove(&args[1])? {
                eprintln!("warning: {w}");
            }
        }
        _ => bail!("{USAGE}"),
    }
    Ok(())
}
