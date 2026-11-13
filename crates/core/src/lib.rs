use anyhow::{anyhow, bail, Context, Result};
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2::AllocAnyThread;
use objc2_app_kit::{NSCompositingOperation, NSImage, NSRunningApplication, NSWorkspace, NSWorkspaceIconCreationOptions};
use block2::RcBlock;
use objc2_foundation::{NSError, NSPoint, NSRect, NSSize, NSString, NSURL};
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

mod dock_ax;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// 本物のバンドル直下に Icon\r を付ける。複製を持たないので更新とのズレが起きない
    InPlace,
    /// 書き込めない本物のための代替。APFS クローンに Icon\r を付けて起動する
    Clone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub mode: Mode,
    pub bundle_id: String,
    pub source_path: PathBuf,
    pub icon_path: PathBuf,
    pub fingerprint: String,
    /// Dock で画像を動かす(実験的)。オンのときだけある
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation: Option<Animation>,
}

/// 本物のタイルは Dock がアイコンを固定してしまうので動かせない。代わりに、Dock のプラグインで自分のタイルを
/// 描き続ける起動用アプリを作ってピン留めする
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Animation {
    pub launcher_path: PathBuf,
    /// 本物がピン留めされていて、その位置を起動用アプリに置き換えた。オフにしたら本物を同じ位置に戻す
    pub replaced_pin: bool,
    /// 起動用アプリが本物の実行ファイルに exec する(Dock から開けばタイルが 1 つで済む)。false なら本物を別に開く
    /// (本物の起動中はタイルが 2 つ並ぶ)。データが保護されたアプリは exec すると自分のデータを読めなくなるので false
    #[serde(default)]
    pub direct: bool,
    /// 起動用アプリを作った時刻(UNIX 秒)。これより前に起動したプロセスは、このタイルには結び付いていない。
    /// ファイルの作成日時は ditto が雛形のものを引き継ぐので使えない
    #[serde(default)]
    pub created: u64,
    /// 本物が起動用アプリを通さずに開かれたら、DockMorph が自動で開き直す
    #[serde(default = "enabled")]
    pub auto_rejoin: bool,
}

fn enabled() -> bool {
    true
}

/// 動くタイルと、起動中の本物の関係
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileState {
    NotRunning,
    /// 起動用アプリから開かれていて、動くタイルがそのまま起動中のタイルになっている
    Joined,
    /// 本物のタイル(静止画)が別に出ている
    Split,
}

/// 起動用アプリが exec の前に立てる印(crates/app/launcher/main.swift と揃える)
const LAUNCHER_ENV: &[u8] = b"\0DOCKMORPH_LAUNCHER=";

pub struct AnimationResult {
    pub entry: Entry,
    /// exec 方式をやめて本物を別に開く方式にした理由。黙って方式が変わらないよう呼び出し側に必ず見せる
    pub fallback_reason: Option<String>,
}

impl Entry {
    pub fn display_name(&self) -> String {
        self.source_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.bundle_id.clone())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SyncResult {
    UpToDate(String),
    Recloned(String),
    Reapplied(String),
    /// 元アプリが移動されていたので bundle ID から探し直した
    Relocated(String, PathBuf),
    SkippedRunning(String),
    /// 元アプリの削除などで 1 件だけ失敗しても、他のエントリの同期は止めない
    Failed(String, String),
}

pub struct AddResult {
    pub entry: Entry,
    /// in-place を試みて失敗しクローンに切り替えた場合の理由。黙って方式が変わらないよう呼び出し側に必ず見せる
    pub fallback_reason: Option<String>,
}

const LSREGISTER: &str =
    "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn LSCopyDefaultHandlerForURLScheme(scheme: *const NSString) -> *mut NSString;
}

/// 起動用アプリが本物の代わりに受け持つ URL スキーム。リンクから開かれても起動用アプリを通るので、タイルが分かれず、
/// exec した本物に URL もそのまま届く(実測)。
/// 書類の種類(UTI)は受け持たない。既定を切り替えると macOS が「すべて〜で開きますか?」の確認を出すことがある(Scotch の .exe で実測)
#[derive(Debug, Clone, PartialEq, Eq)]
struct Scheme(String);

impl Scheme {
    fn default_handler(&self) -> Option<String> {
        let ptr = unsafe { LSCopyDefaultHandlerForURLScheme(&*NSString::from_str(&self.0)) };
        // Copy 規則で +1 の参照が返る
        unsafe { Retained::from_raw(ptr) }.map(|s| s.to_string())
    }

    /// bundle ID で指定する旧 API(LSSetDefault…)は、成功を返しながら何も変えないことがあった(VS Code の json で実測)。
    /// アプリの場所で指定する NSWorkspace の API を使う
    fn set_default_handler(&self, app: &Path) -> Result<()> {
        let ws = NSWorkspace::sharedWorkspace();
        let url = NSURL::fileURLWithPath(&NSString::from_str(&app.to_string_lossy()));
        let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
        let done = RcBlock::new(move |err: *mut NSError| {
            let message = unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string());
            // 受け手が時間切れで先に戻っていたら、届け先がないだけ
            let _ = tx.send(message);
        });
        ws.setDefaultApplicationAtURL_toOpenURLsWithScheme_completionHandler(&url, &NSString::from_str(&self.0), Some(&done));
        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(None) => Ok(()),
            Ok(Some(e)) => bail!("{}: の既定のアプリを切り替えられません ({}): {e}", self.0, app.display()),
            Err(_) => bail!("{}: の既定のアプリの切り替えが終わりません ({})", self.0, app.display()),
        }
    }

    fn handled_by(&self, bundle_id: &str) -> bool {
        self.default_handler().is_some_and(|h| h.eq_ignore_ascii_case(bundle_id))
    }
}

#[derive(Clone)]
pub struct DockMorph {
    home: PathBuf,
    launchers: PathBuf,
}

struct AppInfo {
    bundle_id: String,
    fingerprint: String,
}

/// entries.json の読み書きを GUI・CLI・自動同期の間で直列化する。flock はプロセスが落ちれば OS が外すので、残骸で詰まらない
struct Lock(fs::File);

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl DockMorph {
    /// DOCKMORPH_HOME は検証時に実環境の設定を汚さないための上書き口
    pub fn new() -> Result<Self> {
        Ok(match std::env::var_os("DOCKMORPH_HOME") {
            Some(p) => {
                let home = PathBuf::from(p);
                Self { launchers: home.join("Launchers"), home }
            }
            None => {
                let user = PathBuf::from(std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME が未設定です"))?);
                // Raycast は ~/Library 配下のアプリを候補に出さない(実測)。Spotlight・Raycast から起動用アプリを
                // 開けるよう、アプリの置き場所として扱われる ~/Applications に置く
                Self { home: user.join("Library/Application Support/DockMorph"), launchers: user.join("Applications/DockMorph") }
            }
        })
    }

    fn clones_dir(&self) -> PathBuf { self.home.join("Apps") }
    fn icons_dir(&self) -> PathBuf { self.home.join("Icons") }
    fn launchers_dir(&self) -> PathBuf { self.launchers.clone() }
    fn dock_backups_dir(&self) -> PathBuf { self.home.join("DockBackups") }
    fn entries_file(&self) -> PathBuf { self.home.join("entries.json") }
    /// 更新日時が、最後に Dock のアイコンの記憶を消した時刻
    fn dock_refreshed_marker(&self) -> PathBuf { self.home.join("dock-refreshed") }

    pub fn clone_path(&self, entry: &Entry) -> Result<PathBuf> {
        let name = entry.source_path.file_name().ok_or_else(|| anyhow!("不正なパス: {:?}", entry.source_path))?;
        Ok(self.clones_dir().join(name))
    }

    /// 実際に起動されるバンドル(in-place なら本物、clone なら複製)
    pub fn target_path(&self, entry: &Entry) -> Result<PathBuf> {
        match entry.mode {
            Mode::InPlace => Ok(entry.source_path.clone()),
            Mode::Clone => self.clone_path(entry),
        }
    }

    fn lock(&self) -> Result<Lock> {
        fs::create_dir_all(&self.home)?;
        let f = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.home.join(".lock"))
            .with_context(|| format!("ロックファイルを開けません: {}", self.home.display()))?;
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            bail!("ロックを取れません: {}", std::io::Error::last_os_error());
        }
        Ok(Lock(f))
    }

    /// 書き込みは一時ファイル + rename で原子的に行うので、読むだけならロックは要らない
    pub fn load_entries(&self) -> Result<Vec<Entry>> {
        let f = self.entries_file();
        let bytes = match fs::read(&f) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e).with_context(|| format!("読めません: {}", f.display())),
        };
        serde_json::from_slice(&bytes).with_context(|| format!("壊れています: {}", f.display()))
    }

    fn save_entries(&self, entries: &[Entry]) -> Result<()> {
        let tmp = self.home.join("entries.json.tmp");
        let f = fs::File::create(&tmp)?;
        serde_json::to_writer_pretty(&f, entries)?;
        // 電源断で中身が空の entries.json に置き換わると、付けたアイコンを remove で戻せなくなる
        f.sync_all()?;
        fs::rename(tmp, self.entries_file())?;
        Ok(())
    }

    // 更新検知は版数と実行ファイルの mtime で行う。Sparkle 等は差し替え時に両方か片方を必ず変える
    fn app_info(app: &Path) -> Result<AppInfo> {
        let plist_path = app.join("Contents/Info.plist");
        let info = plist::Value::from_file(&plist_path)
            .with_context(|| format!("アプリとして読めません: {}", app.display()))?;
        let d = info.as_dictionary().ok_or_else(|| anyhow!("Info.plist が辞書ではありません: {}", plist_path.display()))?;
        let s = |k: &str| d.get(k).and_then(|v| v.as_string()).unwrap_or("").to_string();
        let bundle_id = s("CFBundleIdentifier");
        if bundle_id.is_empty() {
            bail!("CFBundleIdentifier がありません: {}", app.display());
        }
        let exec = s("CFBundleExecutable");
        let mtime = fs::metadata(app.join("Contents/MacOS").join(&exec))
            .with_context(|| format!("実行ファイルが見つかりません: {exec}"))?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        Ok(AppInfo { bundle_id, fingerprint: format!("{}|{}|{}", s("CFBundleShortVersionString"), s("CFBundleVersion"), mtime) })
    }

    /// 自分の複製は LaunchServices に同じ bundle ID の別候補として登録されているので、本物を探すときは除外する
    fn locate_app(&self, bundle_id: &str) -> Option<PathBuf> {
        let url = NSWorkspace::sharedWorkspace().URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle_id))?;
        let path = PathBuf::from(url.path()?.to_string());
        (!path.starts_with(self.clones_dir()) && path.join("Contents/Info.plist").exists())
            .then_some(path)
    }

    fn running_app(bundle: &Path) -> Option<Retained<NSRunningApplication>> {
        NSWorkspace::sharedWorkspace().runningApplications().iter().find(|a| {
            a.bundleURL().and_then(|u| u.path()).is_some_and(|p| Path::new(&p.to_string()) == bundle)
        })
    }

    pub fn is_running(bundle: &Path) -> bool {
        Self::running_app(bundle).is_some()
    }

    fn process_start(pid: i32) -> Option<(u64, u64)> {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
        let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
        (n == size).then_some((info.pbi_start_tvsec, info.pbi_start_tvusec))
    }

    /// 引数と環境変数の領域をそのまま探す。アプリが argv を書き換えていても位置に頼らずに済む
    fn launched_by_launcher(pid: i32) -> bool {
        let mut argmax: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>();
        let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
        if unsafe { libc::sysctl(mib.as_mut_ptr(), 2, (&raw mut argmax).cast(), &mut len, std::ptr::null_mut(), 0) } != 0 {
            return false;
        }
        let mut buf = vec![0u8; argmax as usize];
        let mut len = buf.len();
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) } != 0 {
            return false;
        }
        buf[..len].windows(LAUNCHER_ENV.len()).any(|w| w == LAUNCHER_ENV)
    }

    fn dock_start() -> Option<(u64, u64)> {
        let dock = NSWorkspace::sharedWorkspace()
            .runningApplications()
            .iter()
            .find(|a| a.bundleIdentifier().is_some_and(|id| id.to_string() == "com.apple.dock"))?;
        Self::process_start(dock.processIdentifier())
    }

    /// Dock で動かしていない登録は None。
    /// Dock は起動したアプリを、起動した時点のバンドル(起動用アプリ)のタイルに結び付ける。exec 後のプロセスは
    /// LaunchServices 上は本物なので、Dock が再起動すると本物のタイルとして並べ直され、動くタイルから外れる(実測)。
    /// だから「今の起動用アプリから開かれた」かつ「今の Dock より後に起動した」ときだけ 1 つにまとまっている
    pub fn tile_state(&self, entry: &Entry) -> Option<TileState> {
        let anim = entry.animation.as_ref()?;
        let target = self.target_path(entry).ok()?;
        let Some(app) = Self::running_app(&target) else { return Some(TileState::NotRunning) };
        if !anim.direct {
            return Some(TileState::Split);
        }
        let pid = app.processIdentifier();
        // 起動用アプリを作り直す(オフ→オン)と、前の起動用アプリのタイルに結び付いていたプロセスは新しいタイルに移らない(実測)
        let joined = Self::launched_by_launcher(pid)
            && matches!(
                (Self::process_start(pid), Self::dock_start()),
                (Some(app), Some(dock)) if app >= dock && app.0 >= anim.created
            );
        Some(if joined { TileState::Joined } else { TileState::Split })
    }

    /// 本物が起動用アプリを通さずに開かれて、まだ起動した直後なら、そのプロセスの pid を返す。
    /// 自動で開き直してよいのは起動直後だけ。Dock の再起動で分かれた場合など、使っている最中のアプリは対象にしない
    pub fn just_launched_outside(&self, entry: &Entry) -> Option<i32> {
        const FRESH_SECS: u64 = 20;
        entry.animation.as_ref().filter(|a| a.direct && a.auto_rejoin)?;
        let pid = Self::running_app(&self.target_path(entry).ok()?)?.processIdentifier();
        let (started, _) = Self::process_start(pid)?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        (now.saturating_sub(started) <= FRESH_SECS && !Self::launched_by_launcher(pid)).then_some(pid)
    }

    pub fn set_auto_rejoin(&self, bundle_id: &str, on: bool) -> Result<()> {
        let _lock = self.lock()?;
        let mut entries = self.load_entries()?;
        let entry = entries.iter_mut().find(|e| e.bundle_id == bundle_id).ok_or_else(|| anyhow!("未登録です: {bundle_id}"))?;
        let anim = entry.animation.as_mut().ok_or_else(|| anyhow!("Dock で動かす設定になっていません: {bundle_id}"))?;
        anim.auto_rejoin = on;
        self.save_entries(&entries)
    }

    /// 分かれたタイルを 1 つに戻す。起動中のプロセスを後から動くタイルに結び付ける方法はないので、アプリを終了して
    /// 起動用アプリから開き直す
    pub fn rejoin(&self, bundle_id: &str) -> Result<()> {
        let entries = self.load_entries()?;
        let entry = entries.iter().find(|e| e.bundle_id == bundle_id).ok_or_else(|| anyhow!("未登録です: {bundle_id}"))?;
        let anim = entry.animation.as_ref().ok_or_else(|| anyhow!("Dock で動かす設定になっていません: {}", entry.display_name()))?;
        if !anim.direct {
            bail!("このアプリは本物を別に開く方式なので、タイルを 1 つにまとめられません: {}", entry.display_name());
        }
        if let Some(app) = Self::running_app(&self.target_path(entry)?) {
            let pid = app.processIdentifier();
            if !app.terminate() {
                bail!("アプリに終了を頼めません: {}", entry.display_name());
            }
            // runningApplications は実行ループが回らないと更新されないので、プロセスの有無を直接見る
            let exited = (0..100).any(|_| {
                std::thread::sleep(std::time::Duration::from_millis(200));
                (unsafe { libc::kill(pid, 0) }) != 0
            });
            if !exited {
                bail!("{} が終了しませんでした(保存の確認などが出ていないか見てください)。終了したあと、動くタイルか起動用アプリから開き直すと 1 つにまとまります", entry.display_name());
            }
            // 終了した直後は LaunchServices に前のプロセスの登録が残っていて、同じタイルに結び付かないことを避ける
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Self::run(Command::new("/usr/bin/open").arg(&anim.launcher_path))
    }

    /// 同梱の雛形が新しくなったら、作成済みの起動用アプリの実行ファイルとプラグインを入れ替える。入れ替えた登録の名前を返す。
    /// プラグインは Dock が読み込んでいるので、上書きではなく別の実体に差し替える(Dock は再起動まで前のものを使い続ける)
    pub fn upgrade_launchers(&self, template: &Path) -> Result<Vec<String>> {
        // 署名し直すと実行ファイルの中身が変わるので、中身ではなく雛形に付けた版の印で比べる
        const VERSION: &str = "Contents/Resources/dm-template";
        const PARTS: [&str; 3] = ["Contents/MacOS/DMLauncher", "Contents/PlugIns/DMAnimTile.docktileplugin", VERSION];
        let version = fs::read(template.join(VERSION))
            .with_context(|| format!("起動用アプリの雛形が見つかりません: {}", template.display()))?;
        let _lock = self.lock()?;
        let mut upgraded = vec![];
        for entry in self.load_entries()? {
            let Some(anim) = &entry.animation else { continue };
            if !anim.launcher_path.join("Contents/Info.plist").exists() {
                continue; // sync が報告する
            }
            match fs::read(anim.launcher_path.join(VERSION)) {
                Ok(v) if v == version => continue,
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("読めません: {}", anim.launcher_path.display())),
            }
            for rel in PARTS {
                let (from, to) = (template.join(rel), anim.launcher_path.join(rel));
                let tmp = to.with_extension("dm-new");
                if tmp.exists() {
                    fs::remove_dir_all(&tmp).or_else(|_| fs::remove_file(&tmp))?;
                }
                Self::run(Command::new("/usr/bin/ditto").arg(&from).arg(&tmp))?;
                if to.is_dir() {
                    fs::remove_dir_all(&to)?;
                }
                fs::rename(&tmp, &to).with_context(|| format!("起動用アプリを更新できません: {}", to.display()))?;
            }
            if let Err(e) = self.refresh_launcher(&entry, &anim.launcher_path, anim.direct) {
                // 署名まで終わっていないものを「入れ替え済み」と見なさないよう、次回やり直させる
                if let Err(rm) = fs::remove_file(anim.launcher_path.join(VERSION)) {
                    eprintln!("後片付けに失敗 ({}): {rm}", anim.launcher_path.display());
                }
                return Err(e);
            }
            upgraded.push(entry.display_name());
        }
        Ok(upgraded)
    }

    /// Dock はアイコンを自分のキャッシュ(iconcache)に覚えていて、Icon\r を書き換えても、Dock やアプリを再起動しても
    /// 描き直さないことがある(実測)。古いままになるのは、付けた時点で Dock にタイルがあった場合:
    /// ピン留めされている、または付ける前から起動している。最後に Dock を更新した後に付けたものだけが対象
    pub fn dock_stale(&self, entries: &[Entry]) -> Vec<bool> {
        let pinned: Vec<String> = match Self::read_dock() {
            Ok((_, mut dock)) => Self::persistent_apps(&mut dock)
                .map(|apps| {
                    apps.iter()
                        .filter_map(|t| {
                            t.as_dictionary()?.get("tile-data")?.as_dictionary()?.get("file-data")?.as_dictionary()?
                                .get("_CFURLString")?.as_string().map(str::to_string)
                        })
                        .collect()
                })
                .unwrap_or_default(),
            Err(e) => {
                eprintln!("Dock のピン留めを読めません: {e:#}");
                vec![]
            }
        };
        let refreshed = fs::metadata(self.dock_refreshed_marker()).and_then(|m| m.modified()).ok();
        entries
            .iter()
            .map(|entry| {
                let Ok(target) = self.target_path(entry) else { return false };
                let Ok(applied) = fs::metadata(target.join("Icon\r")).and_then(|m| m.modified()) else { return false };
                if refreshed.is_some_and(|r| r >= applied) {
                    return false;
                }
                let is_pinned = Self::file_url(&target).is_ok_and(|u| pinned.contains(&u));
                let running_since_before = Self::running_app(&target).and_then(|a| a.launchDate()).is_some_and(|launched| {
                    let applied = applied.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.);
                    launched.timeIntervalSince1970() < applied
                });
                is_pinned || running_since_before
            })
            .collect()
    }

    /// Dock のアイコンの記憶(iconcache)を消して Dock を再起動する。アプリは終了させない。
    /// Dock の再起動だけでは記憶が残って古いアイコンのままだった(実測)
    pub fn refresh_dock(&self) -> Result<()> {
        let _lock = self.lock()?;
        let cache = Self::dock_icon_cache()?;
        Self::wait_dock_settled();
        match fs::remove_file(&cache) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("Dock のアイコンの記憶を消せません: {}", cache.display())),
        }
        Self::restart_dock()?;
        fs::write(self.dock_refreshed_marker(), b"")?;
        Ok(())
    }

    /// LaunchServices に残った登録が、bundle ID からの検索で消えたパスを返さないようにする。
    /// 公開 API に登録解除がないので lsregister を使う。無くても致命的ではないので記録だけ残す
    fn unregister(path: &Path) {
        match Command::new(LSREGISTER).arg("-u").arg(path).status() {
            Ok(s) if s.success() => {}
            Ok(s) => eprintln!("lsregister -u が失敗 ({s}): {}", path.display()),
            Err(e) => eprintln!("lsregister を実行できません: {e}"),
        }
    }

    fn dock_icon_cache() -> Result<PathBuf> {
        let mut buf = vec![0u8; libc::PATH_MAX as usize];
        let n = unsafe { libc::confstr(libc::_CS_DARWIN_USER_CACHE_DIR, buf.as_mut_ptr().cast(), buf.len()) };
        if n == 0 || n > buf.len() {
            bail!("ユーザーのキャッシュの場所が分かりません: {}", std::io::Error::last_os_error());
        }
        buf.truncate(n - 1);
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&buf)).join("com.apple.dock.iconcache"))
    }

    fn load_image(icon: &Path) -> Result<Retained<NSImage>> {
        let path = NSString::from_str(&icon.to_string_lossy());
        let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &path)
            .ok_or_else(|| anyhow!("画像として読めません: {}", icon.display()))?;
        // 中身が空のファイルや壊れた画像でも NSImage 自体は作れてしまう
        if image.representations().is_empty() {
            bail!("画像として読めません: {}", icon.display());
        }
        Ok(image)
    }

    fn make_clone(source: &Path, dest: &Path, icon: &Path) -> Result<()> {
        fs::create_dir_all(dest.parent().ok_or_else(|| anyhow!("親ディレクトリがありません"))?)?;
        if dest.exists() {
            fs::remove_dir_all(dest)?;
        }
        // 実コピーにフォールバックしない: 容量を食う上、更新追従の前提(安価な再作成)が崩れるため
        let s = CString::new(source.as_os_str().as_bytes())?;
        let d = CString::new(dest.as_os_str().as_bytes())?;
        if unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), 0) } != 0 {
            bail!("クローン失敗 ({}): {} → {}", std::io::Error::last_os_error(), source.display(), dest.display());
        }
        Self::apply_icon(icon, dest)
    }

    /// 正方形でない画像は、そのまま渡すと正方形に引き伸ばされる。縦横比を保って透明な正方形の中央に置く
    fn squared(image: Retained<NSImage>) -> Retained<NSImage> {
        let size = image.size();
        if (size.width - size.height).abs() < 0.5 || size.width <= 0. || size.height <= 0. {
            return image;
        }
        let side = size.width.max(size.height);
        let origin = NSPoint::new((side - size.width) / 2., (side - size.height) / 2.);
        // 描画は setIcon が書き出すときに遅延して呼ばれるので、元画像をブロックに持たせる
        let handler = block2::RcBlock::new(move |_: NSRect| {
            image.drawInRect_fromRect_operation_fraction(NSRect::new(origin, size), NSRect::ZERO, NSCompositingOperation::SourceOver, 1.0);
            Bool::YES
        });
        NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(side, side), false, &handler)
    }

    fn apply_icon(icon: &Path, app: &Path) -> Result<()> {
        let image = Self::squared(Self::load_image(icon)?);
        let ok = NSWorkspace::sharedWorkspace().setIcon_forFile_options(
            Some(&image),
            &NSString::from_str(&app.to_string_lossy()),
            NSWorkspaceIconCreationOptions::empty(),
        );
        if !ok {
            bail!("アイコン設定に失敗 (書き込み権限または App Management の許可が必要な可能性): {}", app.display());
        }
        // setIcon が true を返しても、Finder が読むフラグが立っていなければ表示されない
        if !Self::has_custom_icon(app) {
            // 中途半端な Icon\r を残すと、次回 add の「既にカスタムアイコンあり」判定に引っかかる
            if let Err(e) = Self::clear_icon(app) {
                eprintln!("後片付けに失敗: {e:#}");
            }
            bail!("アイコンを書き込んだのにカスタムアイコンとして認識されていません: {}", app.display());
        }
        Ok(())
    }

    fn clear_icon(app: &Path) -> Result<()> {
        let ok = NSWorkspace::sharedWorkspace().setIcon_forFile_options(
            None,
            &NSString::from_str(&app.to_string_lossy()),
            NSWorkspaceIconCreationOptions::empty(),
        );
        if !ok {
            bail!("アイコンの解除に失敗: {}", app.display());
        }
        Ok(())
    }

    /// FinderInfo の kHasCustomIcon ビット(8バイト目の 0x04)。アプリ更新でバンドルが差し替わると消える
    pub fn has_custom_icon(app: &Path) -> bool {
        let Ok(p) = CString::new(app.as_os_str().as_bytes()) else { return false };
        let mut info = [0u8; 32];
        let n = unsafe {
            libc::getxattr(p.as_ptr(), c"com.apple.FinderInfo".as_ptr(), info.as_mut_ptr().cast(), 32, 0, 0)
        };
        n == 32 && info[8] & 0x04 != 0
    }

    pub fn add(&self, source: &Path, image: &Path, force_clone: bool) -> Result<AddResult> {
        let _lock = self.lock()?;
        let mut entries = self.load_entries()?;
        let info = Self::app_info(source)?;
        if entries.iter().any(|e| e.bundle_id == info.bundle_id) {
            bail!("登録済みです: {} (画像を変えるなら set-image か、画面の「画像を変更」を使ってください)", info.bundle_id);
        }
        // 何かを書き換える前に検証する。途中で失敗して孤立したファイルを残さないため
        Self::load_image(image)?;
        if !force_clone && Self::has_custom_icon(source) {
            // 既存のカスタムアイコンを黙って上書きすると remove で元に戻せなくなる
            bail!("既にカスタムアイコンが設定されています。Finder の「情報を見る」で外してから追加してください: {}", source.display());
        }

        let stored = self.store_image(&info.bundle_id, image)?;

        let mut entry = Entry {
            mode: if force_clone { Mode::Clone } else { Mode::InPlace },
            bundle_id: info.bundle_id,
            source_path: source.to_path_buf(),
            icon_path: stored.clone(),
            fingerprint: info.fingerprint,
            animation: None,
        };
        let result = (|| {
            let mut fallback_reason = None;
            if entry.mode == Mode::InPlace {
                if let Err(e) = Self::apply_icon(&stored, source) {
                    fallback_reason = Some(format!("{e:#}"));
                    entry.mode = Mode::Clone;
                }
            }
            if entry.mode == Mode::Clone {
                let clone = self.clone_path(&entry)?;
                // 複製はファイル名で置くので、同名の別アプリの複製を消して作り直してしまわないよう止める
                if let Some(other) = entries.iter().find(|e| e.mode == Mode::Clone && self.clone_path(e).ok().as_ref() == Some(&clone)) {
                    bail!("同じ名前のアプリ ({}) がクローン方式で登録済みです: {}", other.bundle_id, clone.display());
                }
                Self::make_clone(source, &clone, &stored)?;
            }
            entries.push(entry.clone());
            if let Err(e) = self.save_entries(&entries) {
                self.undo(&entry);
                return Err(e);
            }
            Ok(fallback_reason)
        })();
        match result {
            Ok(fallback_reason) => Ok(AddResult { entry, fallback_reason }),
            Err(e) => {
                if let Err(rm) = fs::remove_file(&stored) {
                    eprintln!("後片付けに失敗 ({}): {rm}", stored.display());
                }
                Err(e)
            }
        }
    }

    /// 差し替えのたびに別名で保存する。同じパスに上書きすると、GUI がパスをキーに持つ画像キャッシュで古い画像を出し続ける
    fn store_image(&self, bundle_id: &str, image: &Path) -> Result<PathBuf> {
        fs::create_dir_all(self.icons_dir())?;
        let ext = image.extension().and_then(|e| e.to_str()).unwrap_or("png").to_lowercase();
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis();
        let stored = self.icons_dir().join(format!("{bundle_id}.{stamp}.{ext}"));
        fs::copy(image, &stored).with_context(|| format!("画像をコピーできません: {}", image.display()))?;
        Ok(stored)
    }

    /// 登録済みのアプリの画像だけを差し替える。登録を外して付け直すと、元のアイコンに戻る瞬間ができて Dock がそれを拾いうる
    pub fn change_image(&self, bundle_id: &str, image: &Path) -> Result<Entry> {
        let _lock = self.lock()?;
        let mut entries = self.load_entries()?;
        let i = entries.iter().position(|e| e.bundle_id == bundle_id).ok_or_else(|| anyhow!("未登録です: {bundle_id}"))?;
        Self::load_image(image)?;
        let target = self.target_path(&entries[i])?;
        if !target.join("Contents/Info.plist").exists() {
            bail!("アイコンを付ける先のアプリが見つかりません。同期してから試してください: {}", target.display());
        }
        let old = entries[i].icon_path.clone();
        let stored = self.store_image(bundle_id, image)?;
        let result = (|| {
            Self::apply_icon(&stored, &target)?;
            entries[i].icon_path = stored.clone();
            if let Err(e) = self.save_entries(&entries) {
                // apply_icon は失敗時に Icon\r を外すので、ここでも前の画像を付け直して登録と揃える
                if let Err(re) = Self::apply_icon(&old, &target) {
                    eprintln!("前の画像に戻せません: {re:#}");
                }
                return Err(e);
            }
            Ok(())
        })();
        if let Err(e) = result {
            if !Self::has_custom_icon(&target) {
                if let Err(re) = Self::apply_icon(&old, &target) {
                    eprintln!("前の画像に戻せません: {re:#}");
                }
            }
            if let Err(rm) = fs::remove_file(&stored) {
                eprintln!("後片付けに失敗 ({}): {rm}", stored.display());
            }
            return Err(e);
        }
        if let Err(rm) = fs::remove_file(&old) {
            if rm.kind() != std::io::ErrorKind::NotFound {
                eprintln!("前の画像を消せません ({}): {rm}", old.display());
            }
        }
        if let Some(anim) = &entries[i].animation {
            self.refresh_launcher(&entries[i], &anim.launcher_path, anim.direct)
                .context("アイコンは変えましたが、Dock の動くタイルを更新できません")?;
        }
        Ok(entries[i].clone())
    }

    /// 登録が保存できなかったときに、付けたアイコンや複製を残さない
    fn undo(&self, entry: &Entry) {
        match entry.mode {
            Mode::InPlace => {
                if let Err(e) = Self::clear_icon(&entry.source_path) {
                    eprintln!("後片付けに失敗: {e:#}");
                }
            }
            Mode::Clone => {
                if let Ok(clone) = self.clone_path(entry) {
                    Self::unregister(&clone);
                    if let Err(e) = fs::remove_dir_all(&clone) {
                        eprintln!("後片付けに失敗 ({}): {e}", clone.display());
                    }
                }
            }
        }
    }

    /// Dock を再起動して動くタイルを外したときは、その理由を返す
    pub fn remove(&self, bundle_id: &str) -> Result<Option<String>> {
        let _lock = self.lock()?;
        let mut entries = self.load_entries()?;
        let i = entries.iter().position(|e| e.bundle_id == bundle_id).ok_or_else(|| anyhow!("未登録です: {bundle_id}"))?;
        let entry = entries[i].clone();
        let mut warning = None;
        if let Some(anim) = &entry.animation {
            warning = self.teardown_animation(&entry, anim)?;
            entries[i].animation = None;
            // 途中で失敗しても、Dock から外したことは登録に残す
            self.save_entries(&entries)?;
        }
        match entry.mode {
            Mode::InPlace => {
                if Self::has_custom_icon(&entry.source_path) {
                    Self::clear_icon(&entry.source_path)?;
                }
            }
            Mode::Clone => {
                let clone = self.clone_path(&entry)?;
                if Self::is_running(&clone) {
                    bail!("クローンが起動中です。終了してから実行してください: {}", clone.display());
                }
                if clone.exists() {
                    Self::unregister(&clone);
                    fs::remove_dir_all(&clone)?;
                }
            }
        }
        if entry.icon_path.exists() {
            fs::remove_file(&entry.icon_path)?;
        }
        entries.remove(i);
        self.save_entries(&entries)?;
        Ok(warning)
    }

    pub fn sync(&self) -> Result<Vec<SyncResult>> {
        let _lock = self.lock()?;
        let mut entries = self.load_entries()?;
        let before = entries.clone();
        let mut results = vec![];
        for entry in entries.iter_mut() {
            let id = entry.bundle_id.clone();
            results.push(match self.sync_entry(entry) {
                Ok(r) => r,
                Err(e) => SyncResult::Failed(id, format!("{e:#}")),
            });
        }
        // 定期実行で毎回書き込まないよう、変化があったときだけ保存する
        if entries != before {
            self.save_entries(&entries)?;
        }
        Ok(results)
    }

    fn sync_entry(&self, entry: &mut Entry) -> Result<SyncResult> {
        let id = entry.bundle_id.clone();
        let mut relocated = None;
        if !entry.source_path.join("Contents/Info.plist").exists() {
            let found = self
                .locate_app(&id)
                .ok_or_else(|| anyhow!("元のアプリが見つかりません (削除または移動された可能性): {}", entry.source_path.display()))?;
            if entry.mode == Mode::Clone && found.file_name() != entry.source_path.file_name() {
                bail!("元のアプリの名前が変わったため追従できません。登録し直してください: {}", found.display());
            }
            entry.source_path = found.clone();
            relocated = Some(found);
        }
        let info = Self::app_info(&entry.source_path)?;
        let result = match entry.mode {
            Mode::InPlace => {
                if Self::has_custom_icon(&entry.source_path) {
                    SyncResult::UpToDate(id.clone())
                } else {
                    Self::apply_icon(&entry.icon_path, &entry.source_path)?;
                    entry.fingerprint = info.fingerprint;
                    SyncResult::Reapplied(id.clone())
                }
            }
            Mode::Clone => {
                let clone = self.clone_path(entry)?;
                let exists = clone.exists();
                if info.fingerprint == entry.fingerprint && exists && Self::has_custom_icon(&clone) {
                    SyncResult::UpToDate(id.clone())
                } else if exists && Self::is_running(&clone) {
                    return Ok(SyncResult::SkippedRunning(id));
                } else {
                    Self::make_clone(&entry.source_path, &clone, &entry.icon_path)?;
                    entry.fingerprint = info.fingerprint;
                    SyncResult::Recloned(id.clone())
                }
            }
        };
        if entry.animation.as_ref().is_some_and(|a| a.launcher_path.parent() != Some(self.launchers_dir().as_path())) {
            self.move_launcher(entry)?;
        }
        if let Some(anim) = &entry.animation {
            if !anim.launcher_path.join("Contents/Info.plist").exists() {
                bail!("Dock で動かすための起動用アプリが見つかりません。「Dock で動かす」をオフにしてからオンにし直してください: {}", anim.launcher_path.display());
            }
            let target = self.target_path(entry)?;
            if Self::launcher_target(&anim.launcher_path).as_deref() != Some(target.as_path()) {
                self.refresh_launcher(entry, &anim.launcher_path, anim.direct)?;
            }
        }
        Ok(match (relocated, result) {
            (Some(path), SyncResult::UpToDate(_)) => SyncResult::Relocated(id, path),
            (_, r) => r,
        })
    }

    // ---- Dock で動かす(実験的) ----

    /// 置き場所が変わる前に作られた起動用アプリを今の置き場所へ移し、Dock のピン留めも付け替える
    fn move_launcher(&self, entry: &mut Entry) -> Result<()> {
        let Some(anim) = entry.animation.as_mut() else { return Ok(()) };
        let name = anim.launcher_path.file_name().ok_or_else(|| anyhow!("不正なパス: {:?}", anim.launcher_path))?;
        let dest = self.launchers_dir().join(name);
        if dest.exists() {
            bail!("起動用アプリを移せません。移し先に同じ名前のものがあります: {}", dest.display());
        }
        fs::create_dir_all(self.launchers_dir())?;
        Self::unregister(&anim.launcher_path);
        fs::rename(&anim.launcher_path, &dest)
            .with_context(|| format!("起動用アプリを移せません: {} → {}", anim.launcher_path.display(), dest.display()))?;
        anim.launcher_path = dest.clone();
        Self::run(Command::new(LSREGISTER).arg("-f").arg(&dest))?;
        let id = Self::launcher_id(&entry.bundle_id);
        let url = Self::file_url(&dest)?;
        self.edit_dock(
            |dm| {
                let (original, mut dock) = Self::read_dock()?;
                let apps = Self::persistent_apps(&mut dock)?;
                let Some(i) = apps.iter().position(|t| Self::tile_matches(t, &id, &url)) else { return Ok(Some(())) };
                if Self::tile_matches(&apps[i], "", &url) {
                    return Ok(Some(())); // すでに新しい場所を指している
                }
                apps[i] = Self::app_tile(&url);
                dm.write_dock(&original, &dock).map(Some)
            },
            |apps| !apps.iter().any(|t| Self::tile_matches(t, &id, "") && !Self::tile_matches(t, "", &url)),
        )?;
        // 新しい場所のアプリは一度起動しないと Dock がプラグインを読み込まない(enable_animation と同じ理由)
        Self::run(Command::new("/usr/bin/open").args(["-W", "-g", "-n"]).arg(&dest).args(["--args", "--dm-register"]))
    }

    fn launcher_id(bundle_id: &str) -> String {
        format!("app.dockmorph.launcher.{bundle_id}")
    }

    fn file_url(path: &Path) -> Result<String> {
        let url = objc2_foundation::NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&path.to_string_lossy()), true);
        Ok(url.absoluteString().ok_or_else(|| anyhow!("URL にできません: {}", path.display()))?.to_string())
    }

    fn launcher_target(launcher: &Path) -> Option<PathBuf> {
        let info = plist::Value::from_file(launcher.join("Contents/Info.plist")).ok()?;
        info.as_dictionary()?.get("DMTargetApp")?.as_string().map(PathBuf::from)
    }

    fn run(cmd: &mut Command) -> Result<()> {
        let output = cmd.output().with_context(|| format!("実行できません: {cmd:?}"))?;
        if !output.status.success() {
            bail!("失敗 ({}): {cmd:?} {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(())
    }

    /// 起動用アプリの Info.plist・タイルの画像・署名・静止画のアイコンを、登録の今の状態に合わせて書き直す
    fn refresh_launcher(&self, entry: &Entry, launcher: &Path, direct: bool) -> Result<()> {
        let id = Self::launcher_id(&entry.bundle_id);
        let mut d = plist::Dictionary::new();
        let s = |v: &str| plist::Value::String(v.to_string());
        d.insert("CFBundleIdentifier".into(), s(&id));
        d.insert("CFBundleName".into(), s(&entry.display_name()));
        d.insert("CFBundleExecutable".into(), s("DMLauncher"));
        d.insert("CFBundlePackageType".into(), s("APPL"));
        d.insert("CFBundleShortVersionString".into(), s("1"));
        d.insert("CFBundleVersion".into(), s("1"));
        d.insert("LSMinimumSystemVersion".into(), s("14.0"));
        d.insert("NSHighResolutionCapable".into(), plist::Value::Boolean(true));
        d.insert("NSDockTilePlugIn".into(), s("DMAnimTile.docktileplugin"));
        d.insert("DMTargetApp".into(), s(&self.target_path(entry)?.to_string_lossy()));
        d.insert("DMTargetBundleID".into(), s(&entry.bundle_id));
        d.insert("DMDirect".into(), plist::Value::Boolean(direct));
        // 本物を別に開く方式では、受け持ってもタイルは分かれるので受け持たない
        let schemes: Vec<Scheme> = if direct {
            Self::declared_schemes(&entry.source_path.join("Contents/Info.plist"))?
                .into_iter()
                // ほかのアプリが既定のものは奪わない。作り直し・入れ替えのときは、すでに受け持っているものも続けて持つ
                .filter(|c| c.handled_by(&entry.bundle_id) || c.handled_by(&id))
                .collect()
        } else {
            vec![]
        };
        if !schemes.is_empty() {
            let mut u = plist::Dictionary::new();
            u.insert("CFBundleURLName".into(), s(&id));
            u.insert("CFBundleTypeRole".into(), s("Editor"));
            u.insert("CFBundleURLSchemes".into(), plist::Value::Array(schemes.iter().map(|c| c.0.as_str().into()).collect()));
            d.insert("CFBundleURLTypes".into(), plist::Value::Array(vec![u.into()]));
        }
        plist::Value::Dictionary(d).to_file_xml(launcher.join("Contents/Info.plist"))?;
        // Dock 側のプラグインが周回ごとに読み直すので、書きかけを読ませないよう rename で置き換える
        let resources = launcher.join("Contents/Resources");
        fs::create_dir_all(&resources)?;
        let tmp = resources.join("tile-image.tmp");
        fs::copy(&entry.icon_path, &tmp).with_context(|| format!("画像をコピーできません: {}", entry.icon_path.display()))?;
        fs::rename(&tmp, resources.join("tile-image"))?;
        // codesign は Icon\r や FinderInfo の付いたバンドルを署名しないので、付け直しの前に一度外す
        if Self::has_custom_icon(launcher) {
            Self::clear_icon(launcher)?;
        }
        // 中のプラグインは雛形の署名のまま触らない。Dock が読み込み中のプラグインの実体を書き換えないため
        Self::run(Command::new("/usr/bin/codesign").args(["--force", "-s", "-"]).arg(launcher))?;
        // プラグインが読み込まれるまでの間や、読み込まれなかったときに出る静止画
        Self::apply_icon(&entry.icon_path, launcher)?;
        Self::run(Command::new(LSREGISTER).arg("-f").arg(launcher))?;
        // 宣言を登録してからでないと、LaunchServices は起動用アプリを既定にできない
        for c in &schemes {
            c.set_default_handler(launcher)?;
        }
        Ok(())
    }

    /// Info.plist が宣言している URL スキーム。http(s) は既定のブラウザの変更になり、macOS が確認を出すので除く
    fn declared_schemes(info_plist: &Path) -> Result<Vec<Scheme>> {
        let plist = match plist::Value::from_file(info_plist) {
            Ok(p) => p,
            Err(e) if e.as_io().is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) => return Ok(vec![]),
            Err(e) => return Err(e).with_context(|| format!("読めません: {}", info_plist.display())),
        };
        let mut schemes = vec![];
        let types = plist.as_dictionary().and_then(|d| d.get("CFBundleURLTypes")).and_then(|v| v.as_array());
        for t in types.into_iter().flatten().filter_map(|t| t.as_dictionary()?.get("CFBundleURLSchemes")?.as_array()).flatten() {
            let Some(s) = t.as_string().map(str::to_lowercase) else { continue };
            let c = Scheme(s.clone());
            if s != "http" && s != "https" && !schemes.contains(&c) {
                schemes.push(c);
            }
        }
        Ok(schemes)
    }

    /// 起動用アプリが受け持っていた URL スキームの既定を本物に返す。起動用アプリを消す前に呼ぶ
    fn release_schemes(entry: &Entry, launcher: &Path) -> Result<()> {
        let id = Self::launcher_id(&entry.bundle_id);
        for c in Self::declared_schemes(&launcher.join("Contents/Info.plist"))? {
            // 切り替えた直後は問い合わせに前の既定が返ることがある(実測)ので、本物が既定に見えても戻しておく。
            // ユーザーがその後ほかのアプリを既定にしたものだけは触らない
            if c.handled_by(&id) || c.handled_by(&entry.bundle_id) {
                c.set_default_handler(&entry.source_path)?;
            }
        }
        Ok(())
    }

    fn read_dock() -> Result<(Vec<u8>, plist::Value)> {
        let output = Command::new("/usr/bin/defaults").args(["export", "com.apple.dock", "-"]).output()?;
        if !output.status.success() {
            bail!("Dock の設定を読めません ({}): {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
        }
        let value = plist::Value::from_reader(std::io::Cursor::new(&output.stdout)).context("Dock の設定を解釈できません")?;
        Ok((output.stdout, value))
    }

    /// 書き換える前の設定を毎回残してから、cfprefsd 経由で書き込んで Dock を再起動する
    fn write_dock(&self, original: &[u8], value: &plist::Value) -> Result<()> {
        let dir = self.dock_backups_dir();
        fs::create_dir_all(&dir)?;
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis();
        fs::write(dir.join(format!("com.apple.dock.{stamp}.plist")), original)?;
        let mut backups: Vec<_> = fs::read_dir(&dir)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        backups.sort();
        for old in backups.iter().take(backups.len().saturating_sub(20)) {
            fs::remove_file(old)?;
        }
        let tmp = self.home.join("dock.plist.tmp");
        value.to_file_xml(&tmp)?;
        let imported = Self::run(Command::new("/usr/bin/defaults").args(["import", "com.apple.dock"]).arg(&tmp));
        fs::remove_file(&tmp)?;
        imported?;
        Self::restart_dock()
    }

    fn restart_dock() -> Result<()> {
        let before = Self::dock_pid();
        Self::run(Command::new("/usr/bin/killall").arg("Dock"))?;
        // launchd が Dock を起こし直してタイルを並べ終えるまで待つ。続けてタイルに対して行う操作を空振りさせないため
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if Self::dock_pid().is_some_and(|p| Some(p) != before) {
                std::thread::sleep(std::time::Duration::from_secs(2));
                return Ok(());
            }
        }
        bail!("Dock が再起動しません")
    }

    /// 起動して間もない Dock は、終了させられるときに自分の状態を書き戻して、直前に書いた設定を消す(実測。
    /// 再起動から数秒以内に書き換えると消え、しばらくたってからなら残った)。起動から一定時間たつまで待つ
    fn wait_dock_settled() {
        const SETTLED_SECS: u64 = 6;
        for _ in 0..30 {
            match Self::dock_uptime_secs() {
                Some(up) if up < SETTLED_SECS => std::thread::sleep(std::time::Duration::from_millis(500)),
                _ => return,
            }
        }
    }

    fn dock_uptime_secs() -> Option<u64> {
        let pid = Self::dock_pid()?;
        let output = Command::new("/bin/ps").args(["-o", "etime=", "-p", &pid.to_string()]).output().ok()?;
        // [[dd-]hh:]mm:ss
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let (days, rest) = match text.split_once('-') {
            Some((d, r)) => (d.parse::<u64>().ok()?, r.to_string()),
            None => (0, text),
        };
        let secs = rest.split(':').try_fold(0u64, |acc, part| part.parse::<u64>().ok().map(|v| acc * 60 + v))?;
        Some(days * 86400 + secs)
    }

    fn dock_pid() -> Option<u32> {
        let output = Command::new("/usr/bin/pgrep").args(["-x", "Dock"]).output().ok()?;
        String::from_utf8_lossy(&output.stdout).lines().next()?.trim().parse().ok()
    }

    fn tile_matches(tile: &plist::Value, bundle_id: &str, url: &str) -> bool {
        let Some(data) = tile.as_dictionary().and_then(|t| t.get("tile-data")).and_then(|v| v.as_dictionary()) else { return false };
        let id = data.get("bundle-identifier").and_then(|v| v.as_string());
        let u = data.get("file-data").and_then(|v| v.as_dictionary()).and_then(|f| f.get("_CFURLString")).and_then(|v| v.as_string());
        id == Some(bundle_id) || u == Some(url)
    }

    fn app_tile(url: &str) -> plist::Value {
        let mut file = plist::Dictionary::new();
        file.insert("_CFURLString".into(), plist::Value::String(url.to_string()));
        file.insert("_CFURLStringType".into(), plist::Value::Integer(15.into()));
        let mut data = plist::Dictionary::new();
        data.insert("file-data".into(), plist::Value::Dictionary(file));
        let mut tile = plist::Dictionary::new();
        tile.insert("tile-data".into(), plist::Value::Dictionary(data));
        tile.insert("tile-type".into(), plist::Value::String("file-tile".into()));
        plist::Value::Dictionary(tile)
    }

    fn persistent_apps(dock: &mut plist::Value) -> Result<&mut Vec<plist::Value>> {
        let d = dock.as_dictionary_mut().ok_or_else(|| anyhow!("Dock の設定が辞書ではありません"))?;
        if !d.contains_key("persistent-apps") {
            d.insert("persistent-apps".into(), plist::Value::Array(vec![]));
        }
        d.get_mut("persistent-apps").and_then(|v| v.as_array_mut()).ok_or_else(|| anyhow!("Dock のピン留めが配列ではありません"))
    }

    /// Dock は再起動の直後などに自分の状態を書き戻すことがあり、書き込んだ設定が消えることがある(実測)。
    /// 書いたあとに読み直して、期待どおりでなければやり直す
    fn edit_dock<T>(&self, edit: impl Fn(&Self) -> Result<Option<T>>, applied: impl Fn(&[plist::Value]) -> bool) -> Result<Option<T>> {
        for _ in 0..3 {
            Self::wait_dock_settled();
            let result = edit(self)?;
            let (_, mut dock) = Self::read_dock()?;
            if applied(Self::persistent_apps(&mut dock)?) {
                return Ok(result);
            }
            eprintln!("Dock の設定が反映されなかったのでやり直します");
        }
        bail!("Dock の設定を書き換えられません(Dock が書き戻してしまいます)")
    }

    /// 本物がピン留めされていればその位置を置き換え、なければ末尾に足す。置き換えたら true
    fn pin_launcher(&self, entry: &Entry, launcher: &Path) -> Result<bool> {
        let launcher_url = Self::file_url(launcher)?;
        let id = Self::launcher_id(&entry.bundle_id);
        let replaced = self.edit_dock(
            |dm| dm.pin_launcher_once(entry, launcher).map(Some),
            |apps| apps.iter().any(|t| Self::tile_matches(t, &id, &launcher_url)),
        )?;
        Ok(replaced.unwrap_or(false))
    }

    fn pin_launcher_once(&self, entry: &Entry, launcher: &Path) -> Result<bool> {
        let (original, mut dock) = Self::read_dock()?;
        let launcher_url = Self::file_url(launcher)?;
        let source_url = Self::file_url(&entry.source_path)?;
        let target_url = Self::file_url(&self.target_path(entry)?)?;
        let apps = Self::persistent_apps(&mut dock)?;
        apps.retain(|t| !Self::tile_matches(t, &Self::launcher_id(&entry.bundle_id), &launcher_url));
        let real = apps.iter().position(|t| {
            Self::tile_matches(t, &entry.bundle_id, &source_url) || Self::tile_matches(t, &entry.bundle_id, &target_url)
        });
        let tile = Self::app_tile(&launcher_url);
        let replaced = match real {
            Some(i) => {
                apps[i] = tile;
                true
            }
            None => {
                apps.push(tile);
                false
            }
        };
        self.write_dock(&original, &dock)?;
        Ok(replaced)
    }

    /// 本物を元の位置に戻す必要がなければ、Dock のタイルのメニューで外す(Dock を再起動しない)。
    /// メニューで外せなかったときは Dock の設定を書き換えて再起動し、その理由を返す
    fn unpin_launcher(&self, entry: &Entry, anim: &Animation) -> Result<Option<String>> {
        let launcher_url = Self::file_url(&anim.launcher_path)?;
        let source_url = Self::file_url(&entry.source_path)?;
        let id = Self::launcher_id(&entry.bundle_id);
        let (_, mut dock) = Self::read_dock()?;
        let apps = Self::persistent_apps(&mut dock)?;
        let pinned = apps.iter().any(|t| Self::tile_matches(t, &id, &launcher_url));
        // メニューではタイルを外せても足せないので、本物を戻すときは書き換えと再起動が要る
        let restore_real = anim.replaced_pin && !apps.iter().any(|t| Self::tile_matches(t, &entry.bundle_id, &source_url));
        if !pinned && !restore_real {
            return Ok(None);
        }
        let mut fallback = None;
        if !restore_real {
            match self.unpin_by_menu(anim, &id, &launcher_url) {
                Ok(()) => return Ok(None),
                Err(e) => {
                    let hint = if dock_ax::trusted() {
                        String::new()
                    } else {
                        "。システム設定 › プライバシーとセキュリティ › アクセシビリティで DockMorph を許可すると、再起動せずに外せます".into()
                    };
                    fallback = Some(format!("Dock のメニューから外せなかったので、Dock を再起動して外しました({e:#}){hint}"));
                }
            }
        }
        self.edit_dock(
            |dm| dm.unpin_launcher_once(entry, anim).map(Some),
            |apps| !apps.iter().any(|t| Self::tile_matches(t, &id, &launcher_url)),
        )?;
        Ok(fallback)
    }

    fn unpin_by_menu(&self, anim: &Animation, id: &str, launcher_url: &str) -> Result<()> {
        let pid = Self::dock_pid().ok_or_else(|| anyhow!("Dock が起動していません"))?;
        if !dock_ax::unpin(pid, &anim.launcher_path)? {
            bail!("Dock に起動用アプリのタイルが見つかりません");
        }
        // Dock は設定ファイルへ数秒遅れて書く(実測で約 4 秒)。書かれる前に続きの処理が設定を読むと古い並びを見る
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(10) {
            let (_, mut dock) = Self::read_dock()?;
            if !Self::persistent_apps(&mut dock)?.iter().any(|t| Self::tile_matches(t, id, launcher_url)) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        bail!("メニューで外しましたが、Dock の設定に反映されません")
    }

    fn unpin_launcher_once(&self, entry: &Entry, anim: &Animation) -> Result<()> {
        let (original, mut dock) = Self::read_dock()?;
        let launcher_url = Self::file_url(&anim.launcher_path)?;
        let source_url = Self::file_url(&entry.source_path)?;
        let apps = Self::persistent_apps(&mut dock)?;
        let at = apps.iter().position(|t| Self::tile_matches(t, &Self::launcher_id(&entry.bundle_id), &launcher_url));
        apps.retain(|t| !Self::tile_matches(t, &Self::launcher_id(&entry.bundle_id), &launcher_url));
        let real_pinned = apps.iter().any(|t| Self::tile_matches(t, &entry.bundle_id, &source_url));
        if anim.replaced_pin && !real_pinned {
            // ユーザーが起動用アプリを Dock から外していたら、元の位置が分からないので末尾に戻す
            let i = at.unwrap_or(apps.len()).min(apps.len());
            apps.insert(i, Self::app_tile(&source_url));
        }
        if at.is_none() && !(anim.replaced_pin && !real_pinned) {
            return Ok(()); // Dock に変更がないなら、再起動でちらつかせない
        }
        self.write_dock(&original, &dock)
    }

    fn teardown_animation(&self, entry: &Entry, anim: &Animation) -> Result<Option<String>> {
        let warning = self.unpin_launcher(entry, anim)?;
        if anim.launcher_path.exists() {
            Self::release_schemes(entry, &anim.launcher_path)?;
            Self::unregister(&anim.launcher_path);
            fs::remove_dir_all(&anim.launcher_path)
                .with_context(|| format!("起動用アプリを消せません: {}", anim.launcher_path.display()))?;
        }
        Ok(warning)
    }

    /// template は DockMorph.app に同梱の Launcher.app(プラグイン入りの雛形)
    /// pin が false なら Dock の並びには触らない(Dock も再起動しない)。起動用アプリを Spotlight などから開くと、
    /// 起動中だけ Dock の右側に動くタイルが出る(実測)
    pub fn enable_animation(&self, bundle_id: &str, template: &Path, pin: bool) -> Result<AnimationResult> {
        let _lock = self.lock()?;
        let mut entries = self.load_entries()?;
        let i = entries.iter().position(|e| e.bundle_id == bundle_id).ok_or_else(|| anyhow!("未登録です: {bundle_id}"))?;
        if entries[i].animation.is_some() {
            bail!("すでにオンです: {}", entries[i].display_name());
        }
        if !template.join("Contents/PlugIns/DMAnimTile.docktileplugin/Contents/MacOS/DMAnimTile").exists() {
            bail!("起動用アプリの雛形が見つかりません。DockMorph.app から起動してください: {}", template.display());
        }
        let name = entries[i].source_path.file_name().ok_or_else(|| anyhow!("不正なパス: {:?}", entries[i].source_path))?;
        let launcher = self.launchers_dir().join(name);
        if let Some(other) = entries.iter().find(|e| e.animation.as_ref().is_some_and(|a| a.launcher_path == launcher)) {
            bail!("同じ名前のアプリ ({}) が Dock で動かす設定になっています: {}", other.bundle_id, launcher.display());
        }
        if launcher.exists() {
            // 登録にないものは、前回の途中終了の残骸
            Self::unregister(&launcher);
            fs::remove_dir_all(&launcher)?;
        }
        fs::create_dir_all(self.launchers_dir())?;
        let fallback_reason = self.protected_data(&entries[i])?.map(|p| {
            format!("このアプリのデータは macOS に保護されていて({})、起動用アプリから直接起動するとアプリが自分のデータを読めなくなります。そのため本物を別に開く方式にしました。本物の起動中は Dock にタイルが 2 つ並びます", p.display())
        });
        let direct = fallback_reason.is_none();
        let result = (|| {
            Self::run(Command::new("/usr/bin/ditto").arg(template).arg(&launcher))?;
            self.refresh_launcher(&entries[i], &launcher, direct)?;
            let replaced_pin = pin && self.pin_launcher(&entries[i], &launcher)?;
            // 一度も起動されていないアプリは、ピン留めしただけでは Dock がプラグインを読み込まない。ピン留めのあとに
            // 一度起動すると読み込む(実測)。本物は開かずにすぐ終了する引数で起動する。
            // ピン留めしないなら、最初に開かれたときに読み込まれるので要らない(実測)
            if pin {
                Self::run(Command::new("/usr/bin/open").args(["-W", "-g", "-n"]).arg(&launcher).args(["--args", "--dm-register"]))?;
            }
            let created = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
            let anim = Animation { launcher_path: launcher.clone(), replaced_pin, direct, created, auto_rejoin: true };
            entries[i].animation = Some(anim.clone());
            if let Err(e) = self.save_entries(&entries) {
                match self.unpin_launcher(&entries[i], &anim) {
                    Ok(Some(w)) => eprintln!("{w}"),
                    Ok(None) => {}
                    Err(u) => eprintln!("Dock を元に戻せません: {u:#}"),
                }
                return Err(e);
            }
            Ok(())
        })();
        if let Err(e) = result {
            if launcher.exists() {
                if let Err(r) = Self::release_schemes(&entries[i], &launcher) {
                    eprintln!("既定のアプリを本物に戻せません: {r:#}");
                }
                Self::unregister(&launcher);
                if let Err(rm) = fs::remove_dir_all(&launcher) {
                    eprintln!("後片付けに失敗 ({}): {rm}", launcher.display());
                }
            }
            return Err(e);
        }
        Ok(AnimationResult { entry: entries[i].clone(), fallback_reason })
    }

    /// 本人以外のアクセスを macOS が止めるデータ(com.apple.macl 付き)を持つアプリか。exec した起動用アプリは
    /// 中身が本物でも本人と認められず、Firefox はプロファイルを読めなくなった(実測)。データの置き場所は
    /// アプリ次第なので、名前の一致する Application Support と entitlement の Group Containers だけを見る推定
    fn protected_data(&self, entry: &Entry) -> Result<Option<PathBuf>> {
        let library = PathBuf::from(std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME が未設定です"))?).join("Library");
        let plist = plist::Value::from_file(entry.source_path.join("Contents/Info.plist"))
            .with_context(|| format!("アプリとして読めません: {}", entry.source_path.display()))?;
        let d = plist.as_dictionary().ok_or_else(|| anyhow!("Info.plist が辞書ではありません"))?;
        let mut names: Vec<String> = ["CFBundleName", "CFBundleDisplayName", "CFBundleExecutable"]
            .iter()
            .filter_map(|k| d.get(*k).and_then(|v| v.as_string()).map(str::to_lowercase))
            .collect();
        names.push(entry.bundle_id.to_lowercase());
        names.push(entry.display_name().to_lowercase());
        let mut candidates = vec![library.join("Containers").join(&entry.bundle_id)];
        // 同じ開発元の別製品のグループ(Microsoft の oneauth など)まで拾うと、使ってもいないデータで誤判定する(VS Code で実測)。
        // アプリが entitlement で名乗ったグループだけを見る
        candidates.extend(Self::app_groups(&entry.source_path)?.into_iter().map(|g| library.join("Group Containers").join(g)));
        let support = library.join("Application Support");
        match fs::read_dir(&support) {
            Ok(items) => {
                for item in items {
                    let path = item?.path();
                    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                    if names.contains(&name) {
                        candidates.push(path);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("読めません: {}", support.display())),
        }
        Ok(candidates.into_iter().find(|p| Self::has_xattr(p, c"com.apple.macl")))
    }

    fn has_xattr(path: &Path, name: &std::ffi::CStr) -> bool {
        let Ok(p) = CString::new(path.as_os_str().as_bytes()) else { return false };
        unsafe { libc::getxattr(p.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0, 0, 0) >= 0 }
    }

    /// 署名の entitlement にある com.apple.security.application-groups
    fn app_groups(app: &Path) -> Result<Vec<String>> {
        let output = Command::new("/usr/bin/codesign")
            .args(["-d", "--entitlements", "-", "--xml"])
            .arg(app)
            .output()
            .context("codesign を実行できません")?;
        // 未署名・entitlement なしでは何も出ない(グループも使えない)
        if !output.status.success() || output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let ent = plist::Value::from_reader_xml(output.stdout.as_slice())
            .with_context(|| format!("entitlement を読めません: {}", app.display()))?;
        Ok(ent
            .as_dictionary()
            .and_then(|d| d.get("com.apple.security.application-groups"))
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_string().map(str::to_string)).collect())
            .unwrap_or_default())
    }

    /// Dock を再起動して外したときは、その理由を返す
    pub fn disable_animation(&self, bundle_id: &str) -> Result<Option<String>> {
        let _lock = self.lock()?;
        let mut entries = self.load_entries()?;
        let i = entries.iter().position(|e| e.bundle_id == bundle_id).ok_or_else(|| anyhow!("未登録です: {bundle_id}"))?;
        let Some(anim) = entries[i].animation.clone() else { bail!("オンになっていません: {}", entries[i].display_name()) };
        let warning = self.teardown_animation(&entries[i], &anim)?;
        entries[i].animation = None;
        self.save_entries(&entries)?;
        Ok(warning)
    }

    /// 元アプリにアイコンが付いているか(clone なら複製に付いているか)
    pub fn is_applied(&self, entry: &Entry) -> bool {
        self.target_path(entry).map(|p| Self::has_custom_icon(&p)).unwrap_or(false)
    }

    pub fn launch(&self, bundle_id: &str) -> Result<()> {
        let entries = self.load_entries()?;
        let entry = entries.iter().find(|e| e.bundle_id == bundle_id).ok_or_else(|| anyhow!("未登録です: {bundle_id}"))?;
        // Dock で動かしているときに本物を直接開くと、本物のタイルが別に出てしまう
        let target = match &entry.animation {
            Some(anim) => anim.launcher_path.clone(),
            None => self.target_path(entry)?,
        };
        // open(1) は LaunchServices の公開 CLI。単一インスタンス判定などの挙動を NSWorkspace 経由と揃えられる
        let output = Command::new("/usr/bin/open").arg(&target).output()?;
        if !output.status.success() {
            bail!(
                "起動に失敗 ({}): {} {}",
                output.status,
                target.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    pub fn reveal(&self, bundle_id: &str) -> Result<()> {
        let entries = self.load_entries()?;
        let entry = entries.iter().find(|e| e.bundle_id == bundle_id).ok_or_else(|| anyhow!("未登録です: {bundle_id}"))?;
        let target = self.target_path(entry)?;
        let status = Command::new("/usr/bin/open").arg("-R").arg(&target).status()?;
        if !status.success() {
            bail!("Finder で表示できません ({status}): {}", target.display());
        }
        Ok(())
    }
}
