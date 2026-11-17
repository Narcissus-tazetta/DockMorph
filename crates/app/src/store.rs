use dockmorph_core::{l, lang, tr, DockMorph, Entry, Language, Settings, SyncResult, TileState};
use crate::icon_guard::{self, Process};
use crate::login;

use gpui::{AppContext, Context, SharedString};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

// 元アプリの更新でアイコンが消えるのを検知して付け直す間隔。FSEvents より単純で、xattr と Info.plist の読み取りだけなので軽い
const SYNC_INTERVAL: Duration = Duration::from_secs(30);
const BANNER_TIMEOUT: Duration = Duration::from_secs(4);
// 本物が直接開かれたのを見つける間隔。起動中のアプリの一覧と、対象アプリのプロセス情報を読むだけ
const AUTO_REJOIN_INTERVAL: Duration = Duration::from_secs(2);
fn icon_guard_failure() -> &'static str {
    tr("Antigravity の起動中のアイコンを保持できません", "Cannot keep the icon of the running Antigravity")
}

#[derive(Clone, Copy, PartialEq)]
pub enum Level {
    Success,
    Info,
    Warn,
    Error,
}

#[derive(Clone)]
pub struct Banner {
    pub id: u64,
    pub level: Level,
    pub title: SharedString,
    pub detail: Option<SharedString>,
}

#[derive(Clone)]
pub struct Item {
    pub entry: Entry,
    pub name: SharedString,
    /// 元アプリ(clone なら複製)に今アイコンが付いているか
    pub applied: bool,
    /// 直近の同期で起きた問題。次に成功すれば消える
    pub issue: Option<SharedString>,
    /// 付けたときに Dock にタイルがあったので、Dock を更新するまで Dock は前のアイコンのまま
    pub dock_stale: bool,
    /// Dock で動かしているときの、動くタイルと起動中の本物の関係
    pub tile: Option<TileState>,
}

impl Item {
    pub fn needs_attention(&self) -> bool {
        self.issue.is_some() || !self.applied || self.dock_stale || self.split()
    }

    /// 1 つにまとまるはずの方式なのに、本物のタイルが別に出ている
    pub fn split(&self) -> bool {
        self.tile == Some(TileState::Split) && self.entry.animation.as_ref().is_some_and(|a| a.direct)
    }
}

pub struct Store {
    dm: DockMorph,
    pub items: Vec<Item>,
    pub banner: Option<Banner>,
    pub last_sync: Option<SystemTime>,
    issues: HashMap<String, SharedString>,
    banner_seq: u64,
    pub ax_trusted: bool,
    pub settings: Settings,
    /// Dock を書き換え中のアプリと、いまの手順。終わるまで他の操作を受け付けない
    pub busy: Option<(String, SharedString)>,
    /// busy の進み具合をどの行に出すか
    pub busy_job: DockJob,
    /// 自動で開き直したプロセス。開き直しても本物が直接開かれ続ける場合に、終了と起動を繰り返さないため
    auto_rejoined: HashMap<String, (i32, std::time::Instant)>,
    icon_guard_target: Option<Process>,
    icon_guard_current: Option<Process>,
    icon_guard_attempted: bool,
    icon_guard_busy: bool,
    icon_guard_issue: Option<SharedString>,
    clearing_leftovers: bool,
    /// 同じ失敗を 30 秒ごとに出し直さないため
    leftover_error: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum DockJob {
    Refresh,
    Animation,
    Rejoin,
}

impl Store {
    /// 同期の定期実行はウィンドウではなくこのエンティティに持たせる。ウィンドウを閉じてメニューバーに常駐している間も止めないため
    pub fn new(dm: DockMorph, settings: Settings, cx: &mut Context<Self>) -> Self {
        let mut store = Store {
            dm, items: vec![], banner: None, last_sync: None, issues: HashMap::new(), banner_seq: 0,
            ax_trusted: DockMorph::accessibility_trusted(), settings, busy: None, busy_job: DockJob::Animation,
            auto_rejoined: HashMap::new(), icon_guard_target: None, icon_guard_current: None,
            icon_guard_attempted: false, icon_guard_busy: false, icon_guard_issue: None,
            clearing_leftovers: false, leftover_error: None,
        };
        store.upgrade_launchers(cx);
        store.sync(false, cx);
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(SYNC_INTERVAL).await;
            if this.update(cx, |this, cx| if this.settings.auto_sync { this.sync(false, cx) }).is_err() {
                break;
            }
        })
        .detach();
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(AUTO_REJOIN_INTERVAL).await;
            if this.update(cx, |this, cx| this.auto_rejoin(cx)).is_err() {
                break;
            }
        })
        .detach();
        store
    }

    /// 本物が起動用アプリを通さずに開かれたら、起動した直後のうちに起動用アプリから開き直す
    fn auto_rejoin(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        // タイルの状態(起動中か、分かれたか)は、30 秒ごとの同期を待たずに画面へ出す
        let mut changed = self.ax_trusted != DockMorph::accessibility_trusted();
        self.ax_trusted = DockMorph::accessibility_trusted();
        for item in &mut self.items {
            let tile = self.dm.tile_state(&item.entry);
            changed |= tile != item.tile;
            item.tile = tile;
        }
        if changed {
            cx.notify();
            // まとめ直したあとや、分かれていたアプリが終了したあとに、本物のタイルが Dock の右側へ残る
            self.clear_leftovers(cx);
        }
        self.update_icon_guard(cx);
        let found = self.items.iter().find_map(|i| {
            let pid = self.dm.just_launched_outside(&i.entry)?;
            if self.auto_rejoined.get(&i.entry.bundle_id).is_some_and(|(previous, _)| *previous == pid) {
                return None;
            }
            Some((i.entry.bundle_id.clone(), pid))
        });
        let Some((id, pid)) = found else { return };
        let now = std::time::Instant::now();
        match self.auto_rejoined.insert(id.clone(), (pid, now)) {
            // 終了を頼んでも終了しなかったプロセス。繰り返し頼まない
            Some((prev, _)) if prev == pid => return,
            // 開き直した直後にまた本物が直接開かれた(起動用アプリが exec に失敗した)なら、終了と起動を繰り返さない
            Some((_, at)) if now.duration_since(at) < Duration::from_secs(15) => {
                let name = self.item(&id).map(|i| i.name.to_string()).unwrap_or_default();
                return self.notify(
                    Level::Warn,
                    l!("{name} を自動でまとめ直せません", "Cannot rejoin {name} automatically"),
                    Some(tr("開き直しても本物が直接開かれます。「Dock で動かす」をオフにしてからオンにし直してください", "The real app still opens directly after reopening. Turn \"Animate in the Dock\" off and on again").into()),
                    cx,
                );
            }
            _ => {}
        }
        self.rejoin(&id, cx);
    }

    /// Dock のメニューを操作するので裏で動かす(Dock が応答しないと数秒待つ)
    fn clear_leftovers(&mut self, cx: &mut Context<Self>) {
        if self.clearing_leftovers || self.busy.is_some() {
            return;
        }
        self.clearing_leftovers = true;
        let dm = self.dm.clone();
        let task = cx.background_spawn(async move { dm.clear_leftover_tiles() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.clearing_leftovers = false;
                match result {
                    Ok(names) => {
                        this.leftover_error = None;
                        if !names.is_empty() {
                            this.notify(Level::Info, tr("Dock に残っていた本物のタイルを消しました", "Removed the leftover tile of the real app from the Dock"), Some(names.join(tr("、", ", "))), cx);
                        }
                    }
                    Err(e) => {
                        let why = format!("{e:#}");
                        if this.leftover_error.as_ref() != Some(&why) {
                            this.notify(Level::Warn, tr("Dock に残った本物のタイルを消せません", "Cannot remove the leftover tile of the real app from the Dock"), Some(why.clone()), cx);
                        }
                        this.leftover_error = Some(why);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn update_icon_guard(&mut self, cx: &mut Context<Self>) {
        if self.icon_guard_busy {
            return;
        }
        let desired = self.items.iter().filter(|_| self.settings.keep_running_icon).find(|i| i.entry.bundle_id == icon_guard::BUNDLE_ID && i.tile == Some(TileState::Joined)).and_then(|i| {
            let (pid, started) = self.dm.running_process(&i.entry)?;
            // SIGUSR1 は起動時の inspector のポートを再利用する。
            let port = if DockMorph::startup_icon_guard(pid) { (20000 + pid % 40000) as u16 } else { 9229 };
            Some(Process { pid, started, port, bundle: self.dm.target_path(&i.entry).ok()?, registry: self.dm.entries_file() })
        });
        if desired == self.icon_guard_target && (self.icon_guard_attempted || (desired.is_none() && self.icon_guard_current.is_none())) {
            return;
        }
        let previous = self.icon_guard_current.clone().filter(|p| Some(p) != desired.as_ref());
        self.icon_guard_target = desired.clone();
        self.icon_guard_attempted = true;
        self.icon_guard_busy = true;
        let completed_target = desired.clone();
        let task = cx.background_spawn(async move {
            if let Some(previous) = previous {
                icon_guard::update(&previous, true)?;
            }
            if let Some(desired) = desired {
                icon_guard::update(&desired, false)?;
            }
            anyhow::Ok(())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.icon_guard_busy = false;
                let had_issue = this.icon_guard_issue.is_some();
                if result.is_ok() {
                    this.icon_guard_current = completed_target;
                }
                this.icon_guard_issue = result.as_ref().err().map(|e| l!("起動中のアイコン上書きを抑止できません: {e:#}。「今すぐ同期」で再試行できます", "Cannot suppress icon overwrites in the running app: {e:#}. Retry with \"Sync Now\"").into());
                if let Err(e) = result {
                    this.notify(Level::Warn, icon_guard_failure(), Some(format!("{e:#}")), cx);
                } else if had_issue && this.banner.as_ref().is_some_and(|b| b.title.as_ref() == icon_guard_failure()) {
                    this.notify(Level::Success, tr("Antigravity の起動中のアイコンを保持しています", "Keeping the icon of the running Antigravity"), None, cx);
                }
                this.reload(cx);
            }).ok();
        }).detach();
    }

    pub fn request_accessibility(&mut self, cx: &mut Context<Self>) {
        self.ax_trusted = DockMorph::request_accessibility();
        cx.notify();
    }

    pub fn set_auto_rejoin(&mut self, bundle_id: &str, on: bool, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            self.notify(Level::Info, tr("Dock の更新中です", "The Dock is being updated"), Some(tr("完了してから操作してください", "Wait until it finishes").into()), cx);
            return ;
        }
        let result = self.dm.set_auto_rejoin(bundle_id, on);
        self.reload(cx);
        if let Err(e) = result {
            self.notify(Level::Error, tr("設定を変えられません", "Cannot change the setting"), Some(format!("{e:#}")), cx);
        }
    }

    /// 保存できたときだけ反映する。保存に失敗したのに画面だけ変わると、次の起動で元に戻って食い違う
    fn save_settings(&mut self, settings: Settings, cx: &mut Context<Self>) -> bool {
        if let Err(e) = self.dm.save_settings(&settings) {
            self.notify(Level::Error, tr("設定を保存できません", "Cannot save the settings"), Some(format!("{e:#}")), cx);
            return false;
        }
        self.settings = settings;
        cx.notify();
        true
    }

    pub fn set_language(&mut self, language: Language, cx: &mut Context<Self>) {
        if self.save_settings(Settings { language: Some(language), ..self.settings.clone() }, cx) {
            lang::set(Some(language));
        }
    }

    pub fn set_auto_sync(&mut self, on: bool, cx: &mut Context<Self>) {
        self.save_settings(Settings { auto_sync: on, ..self.settings.clone() }, cx);
    }

    /// オフにすると、適用済みの抑止は次の更新で元の動作に戻る
    pub fn set_keep_running_icon(&mut self, on: bool, cx: &mut Context<Self>) {
        if !self.save_settings(Settings { keep_running_icon: on, ..self.settings.clone() }, cx) {
            return;
        }
        // オフにしたあとも、前の失敗の表示が対象アプリの行に残らないようにする
        if !on {
            self.icon_guard_issue = None;
        }
        self.icon_guard_attempted = false;
        self.update_icon_guard(cx);
        self.reload(cx);
    }

    pub fn reveal_data_folder(&mut self, cx: &mut Context<Self>) {
        let dir = self.dm.home_dir().to_path_buf();
        if let Err(e) = std::process::Command::new("/usr/bin/open").arg(&dir).status().map_err(anyhow::Error::from) {
            self.notify(Level::Error, tr("Finder で表示できません", "Cannot reveal in Finder"), Some(format!("{}: {e:#}", dir.display())), cx);
        }
    }

    /// 結果はバナーで伝える。警告と失敗は画面を見ていないと気づけないので true を返し、呼び出し側がウィンドウを開く
    pub fn toggle_login(&mut self, cx: &mut Context<Self>) -> bool {
        let (level, title, detail) = match login::toggle() {
            Ok(login::Outcome::Enabled) => (Level::Success, tr("ログイン時に起動します", "DockMorph will launch at login"), None),
            Ok(login::Outcome::Disabled) => (Level::Info, tr("ログイン時に起動しません", "DockMorph will not launch at login"), None),
            Ok(login::Outcome::NeedsApproval) => (
                Level::Warn,
                tr("ログイン項目の許可が必要です", "Login Items approval is required"),
                Some(tr("開いたシステム設定の「ログイン項目」で DockMorph をオンにしてください", "Turn on DockMorph in Login Items in the System Settings that just opened").to_string()),
            ),
            Err(e) => (Level::Error, tr("ログイン項目を変更できません", "Cannot change Login Items"), Some(format!("{e:#}"))),
        };
        self.notify(level, title, detail, cx);
        !matches!(level, Level::Success | Level::Info)
    }

    pub fn data_folder(&self) -> String {
        self.dm.home_dir().display().to_string()
    }

    pub fn issue_count(&self) -> usize {
        self.items.iter().filter(|i| i.needs_attention()).count()
    }

    pub fn item(&self, bundle_id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.entry.bundle_id == bundle_id)
    }

    pub fn notify(&mut self, level: Level, title: impl Into<SharedString>, detail: Option<String>, cx: &mut Context<Self>) {
        self.banner_seq += 1;
        let id = self.banner_seq;
        self.banner = Some(Banner { id, level, title: title.into(), detail: detail.map(Into::into) });
        cx.notify();
        // 失敗と警告は読み終えるまで残す。成功・情報は数秒で消す
        if matches!(level, Level::Success | Level::Info) {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(BANNER_TIMEOUT).await;
                this.update(cx, |this, cx| {
                    if this.banner.as_ref().is_some_and(|b| b.id == id) {
                        this.banner = None;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    pub fn dismiss_banner(&mut self, cx: &mut Context<Self>) {
        self.banner = None;
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        match self.dm.load_entries() {
            Ok(entries) => {
                self.issues.retain(|id, _| entries.iter().any(|e| &e.bundle_id == id));
                let stale = self.dm.dock_stale(&entries);
                self.items = entries
                    .into_iter()
                    .zip(stale)
                    .map(|(entry, dock_stale)| Item {
                        name: entry.display_name().into(),
                        applied: self.dm.is_applied(&entry),
                        dock_stale,
                        tile: self.dm.tile_state(&entry),
                        issue: self.issues.get(&entry.bundle_id).cloned().or_else(|| (entry.bundle_id == icon_guard::BUNDLE_ID).then(|| self.icon_guard_issue.clone()).flatten()),
                        entry,
                    })
                    .collect();
            }
            Err(e) => self.notify(Level::Error, tr("登録を読み込めません", "Cannot load registrations"), Some(format!("{e:#}")), cx),
        }
        cx.notify();
    }

    /// 手動実行は結果を必ず表示する。定期実行は前回から変化があったときだけ表示する(同じ失敗を 30 秒ごとに出し直さない)
    pub fn sync(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            if manual {
                self.notify(Level::Info, tr("Dock の更新中です", "The Dock is being updated"), Some(tr("完了してから同期してください", "Wait until it finishes before syncing").into()), cx);
            }
            return;
        }
        if manual {
            self.icon_guard_attempted = false;
        }
        let results = match self.dm.sync() {
            Ok(r) => r,
            Err(e) => {
                self.notify(Level::Error, tr("同期できません", "Cannot sync"), Some(format!("{e:#}")), cx);
                return;
            }
        };
        self.last_sync = Some(SystemTime::now());
        self.clear_leftovers(cx);
        let previous = std::mem::take(&mut self.issues);
        let mut changed = vec![];
        let mut moved = vec![];
        for r in results {
            match r {
                SyncResult::UpToDate(_) => {}
                SyncResult::Recloned(id) | SyncResult::Reapplied(id) => changed.push(id),
                SyncResult::Relocated(id, path) => moved.push(format!("{id} → {}", path.display())),
                SyncResult::SkippedRunning(id) => {
                    self.issues.insert(id, tr("複製が起動中のため、元アプリの更新を反映できていません。終了すると反映されます", "A clone is running, so the source app's update is not applied yet. It is applied once the clone quits").into());
                }
                SyncResult::Failed(id, why) => {
                    self.issues.insert(id, why.into());
                }
            }
        }
        self.reload(cx);
        let new_issues: Vec<_> = self.issues.iter().filter(|(id, why)| previous.get(*id) != Some(*why)).collect();
        let name = |id: &str| self.item(id).map(|i| i.name.to_string()).unwrap_or_else(|| id.to_string());
        if !new_issues.is_empty() || (manual && !self.issues.is_empty()) {
            let mut lines: Vec<_> = self.issues.iter().map(|(id, why)| format!("{}: {why}", name(id))).collect();
            if !changed.is_empty() {
                lines.push(l!("付け直したアプリ: {}", "Reapplied to: {}", changed.iter().map(|id| name(id)).collect::<Vec<_>>().join(tr("、", ", "))));
            }
            let detail = lines.join("\n");
            self.notify(Level::Warn, l!("{} 件のアプリで問題があります", "{} app(s) have problems", self.issues.len()), Some(detail), cx);
        } else if !previous.is_empty() && self.issues.is_empty() {
            // 残したままの警告バナーが、解消後も出続けないようにする
            self.notify(Level::Success, tr("問題は解消しました", "The problem is resolved"), None, cx);
        } else if !changed.is_empty() {
            let names = changed.iter().map(|id| name(id)).collect::<Vec<_>>().join(tr("、", ", "));
            self.notify(Level::Success, tr("アイコンを付け直しました", "Reapplied icons"), Some(l!("{names}(アップデートなどで外れていました)", "{names} (the icon had been removed, for example by an update)")), cx);
        } else if !moved.is_empty() {
            self.notify(Level::Info, tr("移動されたアプリに追従しました", "Followed an app that was moved"), Some(moved.join("\n")), cx);
        } else if manual && self.icon_guard_issue.is_some() {
            self.notify(Level::Info, tr("起動中のアイコン制御を再試行します", "Retrying the running-icon control"), None, cx);
        } else if manual {
            self.notify(Level::Success, tr("すべて最新です", "Everything is up to date"), None, cx);
        }
    }

    /// 成功したら追加した bundle ID を返す
    pub fn add(&mut self, app: &Path, image: &Path, cx: &mut Context<Self>) -> Option<String> {
        if self.busy.is_some() {
            self.notify(Level::Info, tr("Dock の更新中です", "The Dock is being updated"), Some(tr("完了してから操作してください", "Wait until it finishes").into()), cx);
            return None;
        }
        let result = self.dm.add(app, image, false);
        self.reload(cx);
        match result {
            Ok(r) => {
                let name = r.entry.display_name();
                match r.fallback_reason {
                    Some(why) => self.notify(
                        Level::Warn,
                        l!("{name} はクローン方式で追加しました", "Added {name} using the clone method"),
                        Some(l!("本物のアプリに書き込めなかったため、複製にアイコンを付けています。起動は DockMorph か複製から行ってください。\n理由: {why}", "Could not write to the real app, so the icon is applied to a clone. Launch it from DockMorph or the clone.\nReason: {why}")),
                        cx,
                    ),
                    None if self.item(&r.entry.bundle_id).is_some_and(|i| i.dock_stale) => self.notify(
                        Level::Warn,
                        l!("{name} のアイコンを変えました。Dock には「Dock を更新」で反映されます", "Changed the icon of {name}. Use \"Refresh Dock\" to update it in the Dock"),
                        Some(dock_stale_detail().into()),
                        cx,
                    ),
                    None => self.notify(Level::Success, l!("{name} のアイコンを変えました", "Changed the icon of {name}"), None, cx),
                }
                Some(r.entry.bundle_id)
            }
            Err(e) => {
                self.notify(Level::Error, tr("追加できません", "Cannot add"), Some(format!("{e:#}")), cx);
                None
            }
        }
    }

    pub fn change_image(&mut self, bundle_id: &str, image: &Path, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            self.notify(Level::Info, tr("Dock の更新中です", "The Dock is being updated"), Some(tr("完了してから操作してください", "Wait until it finishes").into()), cx);
            return ;
        }
        let name = self.item(bundle_id).map(|i| i.name.to_string()).unwrap_or_else(|| bundle_id.to_string());
        let result = self.dm.change_image(bundle_id, image);
        self.reload(cx);
        match result {
            Ok(_) if self.item(bundle_id).is_some_and(|i| i.dock_stale) => self.notify(
                Level::Warn,
                l!("{name} の画像を変えました。Dock には「Dock を更新」で反映されます", "Changed the image of {name}. Use \"Refresh Dock\" to update it in the Dock"),
                Some(dock_stale_detail().into()),
                cx,
            ),
            Ok(_) => self.notify(Level::Success, l!("{name} の画像を変えました", "Changed the image of {name}"), None, cx),
            Err(e) => self.notify(Level::Error, l!("{name} の画像を変えられません", "Cannot change the image of {name}"), Some(format!("{e:#}")), cx),
        }
    }

    /// 雛形は DockMorph.app の Resources に同梱している。cargo run で直接起動したときは無いので、その旨を出す
    fn launcher_template() -> Option<std::path::PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.parent()?.parent()?.join("Resources/Launcher.app"))
    }

    /// DockMorph を更新したあと、作成済みの起動用アプリにも新しい実行ファイルとプラグインを行き渡らせる
    fn upgrade_launchers(&mut self, cx: &mut Context<Self>) {
        let Some(template) = Self::launcher_template().filter(|t| t.exists()) else { return };
        match self.dm.upgrade_launchers(&template) {
            Ok(names) if names.is_empty() => {}
            Ok(names) => self.notify(
                Level::Info,
                tr("起動用アプリを更新しました", "Updated the launcher app"),
                Some(l!("{}(起動中のアプリには、次に開き直したときから効きます)", "{} (applies to running apps the next time they are reopened)", names.join(tr("、", ", ")))),
                cx,
            ),
            Err(e) => self.notify(Level::Error, tr("起動用アプリを更新できません", "Cannot update the launcher app"), Some(format!("{e:#}")), cx),
        }
    }

    /// Dock の書き換えは再起動を待つので数秒かかる。画面を止めないよう裏で動かし、その間はほかの操作を止める
    fn run_dock_job<T: Send + 'static>(
        &mut self,
        bundle_id: &str,
        kind: DockJob,
        step: &'static str,
        job: impl FnOnce(&DockMorph) -> anyhow::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, anyhow::Result<T>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some((bundle_id.to_string(), step.into()));
        self.busy_job = kind;
        cx.notify();
        let dm = self.dm.clone();
        let task = cx.background_spawn(async move { job(&dm) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.busy = None;
                this.reload(cx);
                done(this, result, cx);
            })
            .ok();
        })
        .detach();
    }

    /// pin は on のときだけ意味がある。false なら Dock の並びに触らず、Dock も再起動しない
    pub fn set_animation(&mut self, bundle_id: &str, on: bool, pin: bool, cx: &mut Context<Self>) {
        let name = self.item(bundle_id).map(|i| i.name.to_string()).unwrap_or_else(|| bundle_id.to_string());
        let id = bundle_id.to_string();
        let template = Self::launcher_template();
        self.run_dock_job(
            bundle_id,
            DockJob::Animation,
            match (on, pin) {
                (true, true) => tr("Dock に動くタイルを置いています…", "Placing the animated tile in the Dock…"),
                (true, false) => tr("起動用アプリを作っています…", "Creating the launcher app…"),
                (false, _) => tr("動くタイルを外しています…", "Removing the animated tile…"),
            },
            move |dm| {
                if on {
                    let t = template.ok_or_else(|| anyhow::anyhow!("{}", l!("DockMorph の場所が分かりません", "Cannot locate DockMorph")))?;
                    dm.enable_animation(&id, &t, pin).map(|r| (r.fallback_reason, r.pin_note))
                } else {
                    dm.disable_animation(&id).map(|warning| (warning, None))
                }
            },
            move |this, result, cx| match (result.map(|(why, note)| {
                let only_note = why.is_none() && note.is_some();
                let text = [why, note].into_iter().flatten().collect::<Vec<_>>().join("\n");
                (!text.is_empty()).then_some((text, only_note))
            }), on) {
                (Ok(Some((note, true))), true) => this.notify(Level::Warn, l!("{name} を Dock で動かしています", "{name} is animated in the Dock"), Some(note), cx),
                (Ok(None), true) if pin => this.notify(
                    Level::Success,
                    l!("{name} を Dock で動かしています", "{name} is animated in the Dock"),
                    Some(tr("Dock に動くタイルを置きました。ここから開けば、起動中もタイルは 1 つのまま動きます", "Placed the animated tile in the Dock. Open the app from there and the tile stays single and animated while it runs").into()),
                    cx,
                ),
                (Ok(None), true) => this.notify(
                    Level::Success,
                    l!("{name} の起動用アプリを作りました", "Created the launcher app for {name}"),
                    Some(l!("Dock には置いていません。Spotlight や Raycast で「{name}」の起動用アプリを開くか、この画面の「起動」から開くと、起動中は Dock の右側で動きます", "Not placed in the Dock. Open the launcher app for \"{name}\" from Spotlight or Raycast, or use \"Launch\" here, and it animates on the right side of the Dock while running")),
                    cx,
                ),
                (Ok(Some((why, _))), true) => this.notify(Level::Warn, l!("{name} を Dock で動かしています(タイルが 2 つ並ぶ方式)", "{name} is animated in the Dock (two tiles appear side by side)"), Some(why), cx),
                (Ok(None), false) => this.notify(Level::Success, l!("{name} の動くタイルを外しました", "Removed the animated tile of {name}"), None, cx),
                (Ok(Some((why, _))), false) => this.notify(Level::Warn, l!("{name} の動くタイルを外しました", "Removed the animated tile of {name}"), Some(why), cx),
                (Err(e), true) => this.notify(Level::Error, l!("{name} を Dock で動かせません", "Cannot animate {name} in the Dock"), Some(format!("{e:#}")), cx),
                (Err(e), false) => this.notify(Level::Error, l!("{name} の動くタイルを外せません", "Cannot remove the animated tile of {name}"), Some(format!("{e:#}")), cx),
            },
            cx,
        );
    }

    /// 分かれたタイルを 1 つに戻す。アプリを終了して起動用アプリから開き直す
    pub fn rejoin(&mut self, bundle_id: &str, cx: &mut Context<Self>) {
        let name = self.item(bundle_id).map(|i| i.name.to_string()).unwrap_or_else(|| bundle_id.to_string());
        let id = bundle_id.to_string();
        self.run_dock_job(
            bundle_id,
            DockJob::Rejoin,
            tr("アプリを終了して、起動用アプリから開き直しています…", "Quitting the app and reopening it from the launcher app…"),
            move |dm| {
                dm.rejoin(&id)?;
                // 起動したアプリが一覧に載るのを待ってから状態を読み直させる
                std::thread::sleep(Duration::from_secs(2));
                Ok(())
            },
            move |this, result, cx| match result {
                Ok(()) => {
                    this.notify(Level::Success, l!("{name} のタイルを 1 つにまとめました", "Merged the tiles of {name} into one"), None, cx);
                    this.clear_leftovers(cx);
                }
                Err(e) => this.notify(Level::Error, l!("{name} のタイルをまとめられません", "Cannot merge the tiles of {name}"), Some(format!("{e:#}")), cx),
            },
            cx,
        );
    }

    pub fn remove(&mut self, bundle_id: &str, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            self.notify(Level::Info, tr("Dock の更新中です", "The Dock is being updated"), Some(tr("完了してから操作してください", "Wait until it finishes").into()), cx);
            return ;
        }
        let name = self.item(bundle_id).map(|i| i.name.to_string()).unwrap_or_else(|| bundle_id.to_string());
        let id = bundle_id.to_string();
        let done = move |this: &mut Self, result: anyhow::Result<Option<String>>, cx: &mut Context<Self>| match result {
            Ok(None) => this.notify(Level::Success, l!("{name} を元のアイコンに戻しました", "Restored the original icon of {name}"), None, cx),
            Ok(Some(why)) => this.notify(Level::Warn, l!("{name} を元のアイコンに戻しました", "Restored the original icon of {name}"), Some(why), cx),
            Err(e) => this.notify(Level::Error, l!("{name} を元に戻せません", "Cannot restore {name}"), Some(format!("{e:#}")), cx),
        };
        if self.item(bundle_id).is_some_and(|i| i.entry.animation.is_some()) {
            self.run_dock_job(bundle_id, DockJob::Animation, tr("動くタイルを外しています…", "Removing the animated tile…"), move |dm| dm.remove(&id), done, cx);
        } else {
            let result = self.dm.remove(bundle_id);
            self.reload(cx);
            done(self, result, cx);
        }
    }

    pub fn launch(&mut self, bundle_id: &str, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            self.notify(Level::Info, tr("Dock の更新中です", "The Dock is being updated"), Some(tr("完了してから操作してください", "Wait until it finishes").into()), cx);
            return ;
        }
        if let Err(e) = self.dm.launch(bundle_id) {
            return self.notify(Level::Error, tr("起動できません", "Cannot launch"), Some(format!("{e:#}")), cx);
        }
        // タイルの状態(起動中か、1 つにまとまっているか)を、次の自動同期を待たずに出す
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            this.update(cx, |this, cx| this.reload(cx)).ok();
        })
        .detach();
    }

    pub fn refresh_dock(&mut self, bundle_id: &str, cx: &mut Context<Self>) {
        self.run_dock_job(
            bundle_id,
            DockJob::Refresh,
            tr("Dock を再起動しています…", "Restarting the Dock…"),
            |dm| dm.refresh_dock(),
            |this, result, cx| match result {
                Ok(()) => this.notify(Level::Success, tr("Dock を更新しました", "Refreshed the Dock"), None, cx),
                Err(e) => this.notify(Level::Error, tr("Dock を更新できません", "Cannot refresh the Dock"), Some(format!("{e:#}")), cx),
            },
            cx,
        );
    }

    pub fn reveal(&mut self, bundle_id: &str, cx: &mut Context<Self>) {
        if let Err(e) = self.dm.reveal(bundle_id) {
            self.notify(Level::Error, tr("Finder で表示できません", "Cannot reveal in Finder"), Some(format!("{e:#}")), cx);
        }
    }

    pub fn target_path(&self, entry: &Entry) -> Option<std::path::PathBuf> {
        self.dm.target_path(entry).ok()
    }
}

fn dock_stale_detail() -> &'static str {
    tr("Finder ではすでに変わっています。Dock は前のアイコンを覚えていて、アプリや Dock を再起動しても描き直しません", "Finder already shows the new icon. The Dock remembers the old icon and does not redraw it, even if you restart the app or the Dock")
}

/// 時刻だけを表示する。日付をまたぐほど同期が止まっていれば、それ自体が異常なので気付ける
pub fn clock(t: SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as libc::time_t).unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}
