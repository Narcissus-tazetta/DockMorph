use anyhow::{bail, Result};
use dockmorph_core::{lang, tr, DockMorph, Mode, SyncResult, TileState};
use std::path::PathBuf;

const USAGE: &str = "usage:
  dockmorph add [--clone] <App.app> <image>
                                    attach the icon to the real app; fall back to a clone if it is not writable (--clone forces a clone)
  dockmorph list                    show registrations and icon locations
  dockmorph sync                    detect app updates and reapply or rebuild icons
  dockmorph set-image <bundle-id> <image>
                                    replace the image of a registered app
  dockmorph animate <bundle-id> on <Launcher.app> [--no-pin]
  dockmorph animate <bundle-id> off
                                    animate the image in the Dock (experimental). Creates a launcher app and pins it to the Dock.
                                    The Dock restarts only when the real app's pin is replaced.
                                    With --no-pin nothing is pinned; the tile animates on the right side of the Dock only while the app runs from the launcher.
                                    Launcher.app is the template in DockMorph.app/Contents/Resources
  dockmorph rejoin <bundle-id>      when the real app's tile appears separately, restart the app and return to one animated tile
  dockmorph clear-leftovers         remove leftover real-app tiles (not running) of animated apps from the Dock
  dockmorph upgrade-launchers <Launcher.app>
                                    replace existing launcher apps with the new template
  dockmorph refresh-dock            clear the icons the Dock remembers and restart the Dock (apps keep running)
  dockmorph launch <bundle-id>      launch
  dockmorph remove <bundle-id>      restore the icon and delete the registration";

fn main() -> Result<()> {
    // Rust は SIGPIPE を無視するので、`list | head` で読み手が先に閉じると println! が panic する。
    // ほかの CLI と同じく、黙って終了させる
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dm = DockMorph::new()?;
    // 設定ファイルが壊れていても、アイコンを戻す操作はできるようにする
    match dm.settings() {
        Ok(settings) => lang::set(settings.language),
        Err(e) => eprintln!("warning: {e:#}"),
    }
    match args.first().map(String::as_str) {
        Some("add") => {
            let force_clone = args.iter().any(|a| a == "--clone");
            let paths: Vec<&String> = args[1..].iter().filter(|a| *a != "--clone").collect();
            if paths.len() != 2 {
                bail!("{USAGE}");
            }
            let r = dm.add(&PathBuf::from(paths[0]), &PathBuf::from(paths[1]), force_clone)?;
            if let Some(reason) = &r.fallback_reason {
                eprintln!("warning: could not write to the real app, so switched to the clone method: {reason}");
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
                    Some(TileState::Joined) => tr("\tanimated (起動中・タイル 1 つ)", "\tanimated (running, single tile)"),
                    Some(TileState::Split) => tr("\tanimated (起動中・本物のタイルが別に出ている)", "\tanimated (running, the real app's tile is separate)"),
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
                println!("note: the Dock remembers the old icon. Apply it with refresh-dock");
            }
        }
        Some("animate") if (args.len() == 4 || (args.len() == 5 && args[4] == "--no-pin")) && args[2] == "on" => {
            let r = dm.enable_animation(&args[1], &PathBuf::from(&args[3]), args.len() == 4)?;
            if let Some(reason) = &r.fallback_reason {
                eprintln!("warning: {reason}");
            }
            if let Some(note) = &r.pin_note {
                eprintln!("note: {note}");
            }
            if let Some(a) = &r.entry.animation {
                let mode = if a.direct { "direct" } else { "separate" };
                println!("animated {} → {} [{mode}]{}", r.entry.bundle_id, a.launcher_path.display(), if a.replaced_pin { tr(" (ピン留めを置き換え)", " (replaced the pin)") } else { "" });
            }
        }
        Some("animate") if args.len() == 3 && args[2] == "off" => {
            if let Some(w) = dm.disable_animation(&args[1])? {
                eprintln!("warning: {w}");
            }
            println!("stopped {}", args[1]);
        }
        Some("rejoin") if args.len() == 2 => dm.rejoin(&args[1])?,
        Some("clear-leftovers") if args.len() == 1 => {
            for name in dm.clear_leftover_tiles()? {
                println!("cleared {name}");
            }
        }
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
