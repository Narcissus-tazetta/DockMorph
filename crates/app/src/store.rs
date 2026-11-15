use crate::icon_guard::{self, Process};
use dockmorph_core::{DockMorph, Entry, SyncResult, TileState};
use gpui::{AppContext, Context, SharedString};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

// 元アプリの更新でアイコンが消えるのを検知して付け直す間隔。FSEvents より単純で、xattr と Info.plist の読み取りだけなので軽い
const SYNC_INTERVAL: Duration = Duration::from_secs(30);
const BANNER_TIMEOUT: Duration = Duration::from_secs(4);
// 本物が直接開かれたのを見つける間隔。起動中のアプリの一覧と、対象アプリのプロセス情報を読むだけ
const AUTO_REJOIN_INTERVAL: Duration = Duration::from_secs(2);
const ICON_GUARD_FAILURE: &str = "Antigravity の起動中のアイコンを保持できません";

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
    pub fn new(dm: DockMorph, cx: &mut Context<Self>) -> Self {
        let mut store = Store {
            dm, items: vec![], banner: None, last_sync: None, issues: HashMap::new(), banner_seq: 0,
            ax_trusted: DockMorph::accessibility_trusted(), busy: None, busy_job: DockJob::Animation,
            auto_rejoined: HashMap::new(), icon_guard_target: None, icon_guard_current: None,
            icon_guard_attempted: false, icon_guard_busy: false, icon_guard_issue: None,
            clearing_leftovers: false, leftover_error: None,
        };
        store.upgrade_launchers(cx);
        store.sync(false, cx);
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(SYNC_INTERVAL).await;
            if this.update(cx, |this, cx| this.sync(false, cx)).is_err() {
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
                    format!("{name} を自動でまとめ直せません"),
                    Some("開き直しても本物が直接開かれます。「Dock で動かす」をオフにしてからオンにし直してください".into()),
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
                            this.notify(Level::Info, "Dock に残っていた本物のタイルを消しました", Some(names.join("、")), cx);
                        }
                    }
                    Err(e) => {
                        let why = format!("{e:#}");
                        if this.leftover_error.as_ref() != Some(&why) {
                            this.notify(Level::Warn, "Dock に残った本物のタイルを消せません", Some(why.clone()), cx);
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
        let desired = self.items.iter().find(|i| i.entry.bundle_id == icon_guard::BUNDLE_ID && i.tile == Some(TileState::Joined)).and_then(|i| {
            let (pid, started) = self.dm.running_process(&i.entry)?;
            Some(Process { pid, started, bundle: self.dm.target_path(&i.entry).ok()?, registry: self.dm.entries_file() })
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
                this.icon_guard_issue = result.as_ref().err().map(|e| format!("起動中のアイコン上書きを抑止できません: {e:#}。「今すぐ同期」で再試行できます").into());
                if let Err(e) = result {
                    this.notify(Level::Warn, ICON_GUARD_FAILURE, Some(format!("{e:#}")), cx);
                } else if had_issue && this.banner.as_ref().is_some_and(|b| b.title.as_ref() == ICON_GUARD_FAILURE) {
                    this.notify(Level::Success, "Antigravity の起動中のアイコンを保持しています", None, cx);
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
            self.notify(Level::Info, "Dock の更新中です", Some("完了してから操作してください".into()), cx);
            return ;
        }
        let result = self.dm.set_auto_rejoin(bundle_id, on);
        self.reload(cx);
        if let Err(e) = result {
            self.notify(Level::Error, "設定を変えられません", Some(format!("{e:#}")), cx);
        }
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
            Err(e) => self.notify(Level::Error, "登録を読み込めません", Some(format!("{e:#}")), cx),
        }
        cx.notify();
    }

    /// 手動実行は結果を必ず表示する。定期実行は前回から変化があったときだけ表示する(同じ失敗を 30 秒ごとに出し直さない)
    pub fn sync(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            if manual {
                self.notify(Level::Info, "Dock の更新中です", Some("完了してから同期してください".into()), cx);
            }
            return;
        }
        if manual {
            self.icon_guard_attempted = false;
        }
        let results = match self.dm.sync() {
            Ok(r) => r,
            Err(e) => {
                self.notify(Level::Error, "同期できません", Some(format!("{e:#}")), cx);
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
                    self.issues.insert(id, "複製が起動中のため、元アプリの更新を反映できていません。終了すると反映されます".into());
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
                lines.push(format!("付け直したアプリ: {}", changed.iter().map(|id| name(id)).collect::<Vec<_>>().join("、")));
            }
            let detail = lines.join("\n");
            self.notify(Level::Warn, format!("{} 件のアプリで問題があります", self.issues.len()), Some(detail), cx);
        } else if !previous.is_empty() && self.issues.is_empty() {
            // 残したままの警告バナーが、解消後も出続けないようにする
            self.notify(Level::Success, "問題は解消しました", None, cx);
        } else if !changed.is_empty() {
            let names = changed.iter().map(|id| name(id)).collect::<Vec<_>>().join("、");
            self.notify(Level::Success, "アイコンを付け直しました", Some(format!("{names}(アップデートなどで外れていました)")), cx);
        } else if !moved.is_empty() {
            self.notify(Level::Info, "移動されたアプリに追従しました", Some(moved.join("\n")), cx);
        } else if manual && self.icon_guard_issue.is_some() {
            self.notify(Level::Info, "起動中のアイコン制御を再試行します", None, cx);
        } else if manual {
            self.notify(Level::Success, "すべて最新です", None, cx);
        }
    }

    /// 成功したら追加した bundle ID を返す
    pub fn add(&mut self, app: &Path, image: &Path, cx: &mut Context<Self>) -> Option<String> {
        if self.busy.is_some() {
            self.notify(Level::Info, "Dock の更新中です", Some("完了してから操作してください".into()), cx);
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
                        format!("{name} はクローン方式で追加しました"),
                        Some(format!("本物のアプリに書き込めなかったため、複製にアイコンを付けています。起動は DockMorph か複製から行ってください。\n理由: {why}")),
                        cx,
                    ),
                    None if self.item(&r.entry.bundle_id).is_some_and(|i| i.dock_stale) => self.notify(
                        Level::Warn,
                        format!("{name} のアイコンを変えました。Dock には「Dock を更新」で反映されます"),
                        Some(DOCK_STALE_DETAIL.into()),
                        cx,
                    ),
                    None => self.notify(Level::Success, format!("{name} のアイコンを変えました"), None, cx),
                }
                Some(r.entry.bundle_id)
            }
            Err(e) => {
                self.notify(Level::Error, "追加できません", Some(format!("{e:#}")), cx);
                None
            }
        }
    }

    pub fn change_image(&mut self, bundle_id: &str, image: &Path, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            self.notify(Level::Info, "Dock の更新中です", Some("完了してから操作してください".into()), cx);
            return ;
        }
        let name = self.item(bundle_id).map(|i| i.name.to_string()).unwrap_or_else(|| bundle_id.to_string());
        let result = self.dm.change_image(bundle_id, image);
        self.reload(cx);
        match result {
            Ok(_) if self.item(bundle_id).is_some_and(|i| i.dock_stale) => self.notify(
                Level::Warn,
                format!("{name} の画像を変えました。Dock には「Dock を更新」で反映されます"),
                Some(DOCK_STALE_DETAIL.into()),
                cx,
            ),
            Ok(_) => self.notify(Level::Success, format!("{name} の画像を変えました"), None, cx),
            Err(e) => self.notify(Level::Error, format!("{name} の画像を変えられません"), Some(format!("{e:#}")), cx),
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
                "起動用アプリを更新しました",
                Some(format!("{}(起動中のアプリには、次に開き直したときから効きます)", names.join("、"))),
                cx,
            ),
            Err(e) => self.notify(Level::Error, "起動用アプリを更新できません", Some(format!("{e:#}")), cx),
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
                (true, true) => "Dock に動くタイルを置いています…",
                (true, false) => "起動用アプリを作っています…",
                (false, _) => "動くタイルを外しています…",
            },
            move |dm| {
                if on {
                    let t = template.ok_or_else(|| anyhow::anyhow!("DockMorph の場所が分かりません"))?;
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
                (Ok(Some((note, true))), true) => this.notify(Level::Warn, format!("{name} を Dock で動かしています"), Some(note), cx),
                (Ok(None), true) if pin => this.notify(
                    Level::Success,
                    format!("{name} を Dock で動かしています"),
                    Some("Dock に動くタイルを置きました。ここから開けば、起動中もタイルは 1 つのまま動きます".into()),
                    cx,
                ),
                (Ok(None), true) => this.notify(
                    Level::Success,
                    format!("{name} の起動用アプリを作りました"),
                    Some(format!("Dock には置いていません。Spotlight や Raycast で「{name}」の起動用アプリを開くか、この画面の「起動」から開くと、起動中は Dock の右側で動きます")),
                    cx,
                ),
                (Ok(Some((why, _))), true) => this.notify(Level::Warn, format!("{name} を Dock で動かしています(タイルが 2 つ並ぶ方式)"), Some(why), cx),
                (Ok(None), false) => this.notify(Level::Success, format!("{name} の動くタイルを外しました"), None, cx),
                (Ok(Some((why, _))), false) => this.notify(Level::Warn, format!("{name} の動くタイルを外しました"), Some(why), cx),
                (Err(e), true) => this.notify(Level::Error, format!("{name} を Dock で動かせません"), Some(format!("{e:#}")), cx),
                (Err(e), false) => this.notify(Level::Error, format!("{name} の動くタイルを外せません"), Some(format!("{e:#}")), cx),
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
            "アプリを終了して、起動用アプリから開き直しています…",
            move |dm| {
                dm.rejoin(&id)?;
                // 起動したアプリが一覧に載るのを待ってから状態を読み直させる
                std::thread::sleep(Duration::from_secs(2));
                Ok(())
            },
            move |this, result, cx| match result {
                Ok(()) => {
                    this.notify(Level::Success, format!("{name} のタイルを 1 つにまとめました"), None, cx);
                    this.clear_leftovers(cx);
                }
                Err(e) => this.notify(Level::Error, format!("{name} のタイルをまとめられません"), Some(format!("{e:#}")), cx),
            },
            cx,
        );
    }

    pub fn remove(&mut self, bundle_id: &str, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            self.notify(Level::Info, "Dock の更新中です", Some("完了してから操作してください".into()), cx);
            return ;
        }
        let name = self.item(bundle_id).map(|i| i.name.to_string()).unwrap_or_else(|| bundle_id.to_string());
        let id = bundle_id.to_string();
        let done = move |this: &mut Self, result: anyhow::Result<Option<String>>, cx: &mut Context<Self>| match result {
            Ok(None) => this.notify(Level::Success, format!("{name} を元のアイコンに戻しました"), None, cx),
            Ok(Some(why)) => this.notify(Level::Warn, format!("{name} を元のアイコンに戻しました"), Some(why), cx),
            Err(e) => this.notify(Level::Error, format!("{name} を元に戻せません"), Some(format!("{e:#}")), cx),
        };
        if self.item(bundle_id).is_some_and(|i| i.entry.animation.is_some()) {
            self.run_dock_job(bundle_id, DockJob::Animation, "動くタイルを外しています…", move |dm| dm.remove(&id), done, cx);
        } else {
            let result = self.dm.remove(bundle_id);
            self.reload(cx);
            done(self, result, cx);
        }
    }

    pub fn launch(&mut self, bundle_id: &str, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            self.notify(Level::Info, "Dock の更新中です", Some("完了してから操作してください".into()), cx);
            return ;
        }
        if let Err(e) = self.dm.launch(bundle_id) {
            return self.notify(Level::Error, "起動できません", Some(format!("{e:#}")), cx);
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
            "Dock を再起動しています…",
            |dm| dm.refresh_dock(),
            |this, result, cx| match result {
                Ok(()) => this.notify(Level::Success, "Dock を更新しました", None, cx),
                Err(e) => this.notify(Level::Error, "Dock を更新できません", Some(format!("{e:#}")), cx),
            },
            cx,
        );
    }

    pub fn reveal(&mut self, bundle_id: &str, cx: &mut Context<Self>) {
        if let Err(e) = self.dm.reveal(bundle_id) {
            self.notify(Level::Error, "Finder で表示できません", Some(format!("{e:#}")), cx);
        }
    }

    pub fn target_path(&self, entry: &Entry) -> Option<std::path::PathBuf> {
        self.dm.target_path(entry).ok()
    }
}

const DOCK_STALE_DETAIL: &str = "Finder ではすでに変わっています。Dock は前のアイコンを覚えていて、アプリや Dock を再起動しても描き直しません";

/// 時刻だけを表示する。日付をまたぐほど同期が止まっていれば、それ自体が異常なので気付ける
pub fn clock(t: SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as libc::time_t).unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}
