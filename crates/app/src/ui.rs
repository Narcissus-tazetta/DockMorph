use dockmorph_core::{l, lang, tr, DockMorph, Language, Mode, TileState};
use crate::assets::icon;
use crate::gif::Gifs;
use crate::store::{clock, BannerAction, DockJob, Item, Level, Store};
use crate::theme::Theme;
use crate::{login, updater};
use crate::{AddApp, CloseWindow, SyncNow};

use std::sync::Arc;
use gpui::{
    deferred, div, img, linear_color_stop, linear_gradient, prelude::*, px, svg, AnyElement, BoxShadow, ClickEvent, Context, MouseDownEvent, Div,
    ElementId, Entity, ExternalPaths, FocusHandle, FontWeight, Hsla, PathPromptOptions, Pixels, PromptButton, PromptLevel, SharedString, Stateful,
    Subscription, Window, point,
};
use std::path::{Path, PathBuf};

// gpui の img() が読める形式。NSImage はもっと多く読めるが、プレビューできないものは受け付けない
const IMAGE_EXTS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];
const SIDEBAR_WIDTH: f32 = 236.;
// 透明タイトルバーの高さ。信号機ボタンとこの帯が重なる
const TITLEBAR: f32 = 44.;

#[derive(Clone, Copy, PartialEq)]
enum PickFor {
    App,
    NewImage,
    ChangeImage,
}

fn is_image(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
    IMAGE_EXTS.contains(&ext.as_str())
}

#[derive(Clone, PartialEq)]
enum Pane {
    Add,
    Settings,
    Entry(String),
}

pub struct MainView {
    store: Entity<Store>,
    pane: Pane,
    pending_app: Option<PathBuf>,
    pending_app_icon: Option<Arc<gpui::Image>>,
    pending_image: Option<PathBuf>,
    /// ファイル選択パネルは開いている間もウィンドウのクリックを受け付けるので、二重に開かないようにする
    picking: bool,
    language_menu_open: bool,
    gifs: Gifs,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl MainView {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.observe(&store, |this, store, cx| {
                // 選んでいたアプリが CLI などから外されたら、残っている先頭に移す
                if let Pane::Entry(id) = &this.pane {
                    if store.read(cx).item(id).is_none() {
                        this.pane = Self::default_pane(store.read(cx));
                    }
                }
                cx.notify();
            }),
            cx.observe_window_appearance(window, |_, _, cx| cx.notify()),
        ];
        let pane = Self::default_pane(store.read(cx));
        let focus = cx.focus_handle();
        window.focus(&focus);
        MainView { store, pane, pending_app: None, pending_app_icon: None, pending_image: None, picking: false, language_menu_open: false, gifs: Gifs::new(), focus, _subscriptions: subscriptions }
    }

    /// 登録画像のプレビュー。GIF は gpui に任せず自前でコマを選ぶ(gif.rs)
    fn picture(&self, path: &Path, id: impl Into<ElementId>, size: f32, radius: f32, cx: &mut Context<Self>) -> AnyElement {
        match self.gifs.source(path, |v: &mut MainView| &v.gifs, cx) {
            Some(src) => img(src).id(id).size(px(size)).flex_none().rounded(px(radius)).into_any_element(),
            None => div().size(px(size)).flex_none().into_any_element(),
        }
    }

    fn default_pane(store: &Store) -> Pane {
        store.items.first().map(|i| Pane::Entry(i.entry.bundle_id.clone())).unwrap_or(Pane::Add)
    }

    pub fn show_settings(&mut self, cx: &mut Context<Self>) {
        self.pane = Pane::Settings;
        cx.notify();
    }

    pub fn show_add(&mut self, cx: &mut Context<Self>) {
        self.pane = Pane::Add;
        cx.notify();
    }

    fn notify_error(&self, title: &str, detail: String, cx: &mut Context<Self>) {
        let title = title.to_string();
        self.store.update(cx, |s, cx| s.notify(Level::Error, title, Some(detail), cx));
    }

    fn accept_app(&mut self, path: &Path, cx: &mut Context<Self>) {
        if path.extension().and_then(|e| e.to_str()) == Some("app") && path.is_dir() {
            self.pending_app = Some(path.to_path_buf());
            self.pending_app_icon = match DockMorph::app_icon_png(path) {
                Ok(png) => Some(Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, png))),
                Err(e) => {
                    self.notify_error(tr("アプリのアイコンを表示できません", "Cannot show the app icon"), format!("{e:#}"), cx);
                    None
                }
            };
        } else {
            self.notify_error(tr("アプリ(.app)ではありません", "Not an app (.app)"), path.display().to_string(), cx);
        }
    }

    fn check_image(&self, path: &Path, cx: &mut Context<Self>) -> bool {
        let ok = is_image(path);
        if !ok {
            self.notify_error(tr("対応していない画像形式です", "Unsupported image format"), l!("png / jpg / webp / gif を選んでください: {}", "Choose png / jpg / webp / gif: {}", path.display()), cx);
        }
        ok
    }

    fn accept_image(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.check_image(path, cx) {
            self.pending_image = Some(path.to_path_buf());
        }
    }

    fn change_image(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Pane::Entry(id) = self.pane.clone() else { return };
        if self.store.read(cx).busy.is_some() {
            return self.notify_error(tr("Dock の更新中です", "The Dock is being updated"), tr("終わってから画像を変えてください", "Change the image after it finishes").into(), cx);
        }
        if self.check_image(path, cx) {
            self.store.update(cx, |s, cx| s.change_image(&id, path, cx));
        }
    }

    fn drop_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        // 詳細画面に画像を 1 枚だけ落としたら、そのアプリの画像の差し替えとして扱う
        if matches!(self.pane, Pane::Entry(_)) && paths.len() == 1 && paths[0].extension().and_then(|e| e.to_str()) != Some("app") {
            self.change_image(&paths[0], cx);
            return cx.notify();
        }
        self.pane = Pane::Add;
        for p in paths {
            if p.extension().and_then(|e| e.to_str()) == Some("app") {
                self.accept_app(p, cx);
            } else {
                self.accept_image(p, cx);
            }
        }
        cx.notify();
    }

    fn pick(&mut self, target: PickFor, cx: &mut Context<Self>) {
        if self.picking {
            return;
        }
        self.picking = true;
        let rx = if target == PickFor::App {
            crate::panel::pick_file(tr("アプリを選択", "Choose App"), Path::new("/Applications"))
        } else {
            cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some(tr("画像を選択", "Choose Image").into()) })
        };
        cx.spawn(async move |this, cx| {
            let result = rx.await;
            this.update(cx, |this, cx| {
                this.picking = false;
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(p) = paths.first() {
                            match target {
                                PickFor::App => this.accept_app(p, cx),
                                PickFor::NewImage => this.accept_image(p, cx),
                                PickFor::ChangeImage => this.change_image(p, cx),
                            }
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(e)) => this.notify_error(tr("ファイルを選べません", "Cannot choose a file"), format!("{e:#}"), cx),
                    Err(e) => this.notify_error(tr("ファイル選択が中断されました", "File selection was interrupted"), e.to_string(), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn confirm_animation(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Warning,
            tr("Dock で画像を動かしますか?(実験的)", "Animate the image in the Dock? (experimental)"),
            Some(tr(
                "動くタイルを表示する小さな起動用アプリを作ります。起動用アプリから開けば、起動中もタイルは 1 つのまま動きます。\n\n\
                 「Dock に置く」: 起動用アプリを Dock にピン留めします。このアプリがピン留めされていれば、その位置を置き換えます。\
                 設定のとき Dock が一瞬消えて再起動します(変更前の Dock の設定は保存します)。\
                 ほかの動くタイルが起動中なら、分けないよう Dock を再起動せずに置きます(タイルは Dock が次に再起動するまで右側に出ます)\n\n\
                 「Dock に置かない」: Dock の並びは変えません。Spotlight・Raycast・この画面から起動用アプリを開くと、起動中だけ Dock の右側で動きます\n\n\
                 ・本物を直接開いたときや、起動後に Dock が再起動したときは、本物のタイル(静止画)が別に出ます。この画面の「まとめ直す」で戻せます\n\
                 ・データが macOS に保護されているアプリ(Firefox など)は、本物を別に開く方式になり、起動中はタイルが 2 つ並びます",
                "DockMorph creates a small launcher app that shows the animated tile. If you open the app from the launcher, the tile stays single and animated while the app runs.\n\n\
                 \"Place in Dock\": pins the launcher app to the Dock. If this app is pinned, the launcher replaces it in the same position. \
                 The Dock disappears briefly and restarts while this is set up (the Dock settings are saved beforehand). \
                 If another animated tile is running, the launcher is placed without restarting the Dock so that tile does not split (the tile stays on the right side until the Dock next restarts).\n\n\
                 \"Don't Place in Dock\": leaves the Dock layout alone. If you open the launcher app from Spotlight, Raycast, or this window, it animates on the right side of the Dock only while the app runs.\n\n\
                 · If the real app is opened directly, or the Dock restarts after it launched, the real tile (still image) appears separately. \"Rejoin\" in this window restores it.\n\
                 · Apps whose data is protected by macOS (such as Firefox) are opened separately, so two tiles appear while they run.",
            )),
            &[PromptButton::ok(tr("Dock に置く", "Place in Dock")), PromptButton::new(tr("Dock に置かない", "Don't Place in Dock")), PromptButton::cancel(tr("キャンセル", "Cancel"))],
            cx,
        );
        cx.spawn(async move |this, cx| {
            let pin = match answer.await {
                Ok(0) => true,
                Ok(1) => false,
                _ => return,
            };
            this.update(cx, |this, cx| this.store.update(cx, |s, cx| s.set_animation(&id, true, pin, cx))).ok();
        })
        .detach();
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        let (Some(app), Some(image)) = (self.pending_app.clone(), self.pending_image.clone()) else { return };
        let view = cx.entity().downgrade();
        self.store.update(cx, |s, cx| {
            s.add(&app, &image, move |id, cx| {
                view.update(cx, |this, cx| {
                    this.pending_app = None;
                    this.pending_app_icon = None;
                    this.pending_image = None;
                    this.pane = Pane::Entry(id);
                    cx.notify();
                })
                .ok();
            }, cx)
        });
    }

    fn confirm_restore_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Critical,
            tr("すべて元に戻して DockMorph を終了しますか?", "Restore everything and quit DockMorph?"),
            Some(tr(
                "登録したすべてのアプリを元のアイコンに戻し、Dock の動くタイルと起動用アプリを外して、ログイン項目からも外します。                 そのあと DockMorph.app をゴミ箱に入れれば、アンインストールは完了です。登録と画像はデータの場所に残ります(不要なら消してください)",
                "This restores the original icons of all registered apps, removes the animated Dock tiles and launcher apps, and removes DockMorph from Login Items.                  Then move DockMorph.app to the Trash to finish uninstalling. Registrations and images stay in the data folder (delete it if you no longer need it)",
            )),
            &[PromptButton::ok(tr("元に戻して終了", "Restore and Quit")), PromptButton::cancel(tr("キャンセル", "Cancel"))],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if !matches!(answer.await, Ok(0)) {
                return;
            }
            this.update(cx, |this, cx| this.store.update(cx, |s, cx| s.restore_all(|cx| cx.quit(), cx))).ok();
        })
        .detach();
    }
}

// ---- 部品 ----

fn symbol(name: &str, size: f32, color: Hsla) -> impl IntoElement {
    svg().path(icon(name)).size(px(size)).flex_none().text_color(color)
}

/// システム設定のサイドバーと同じ、色付きの角丸に白い記号
fn tile(name: &str, tint: Hsla, size: f32) -> impl IntoElement {
    let top = Hsla { l: (tint.l + 0.08).min(1.), ..tint };
    div()
        .size(px(size))
        .flex_none()
        .rounded(px(size * 0.26))
        .bg(linear_gradient(180., linear_color_stop(top, 0.), linear_color_stop(tint, 1.)))
        .flex()
        .items_center()
        .justify_center()
        .child(symbol(name, size * 0.62, gpui::white()))
}

fn pill(text: impl Into<SharedString>, name: &str, tint: Hsla) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .px(px(8.))
        .py(px(2.))
        .rounded_full()
        .bg(Theme::tint(tint, 0.14))
        .text_color(tint)
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(symbol(name, 12., tint))
        .child(text.into())
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Primary,
    Normal,
    Destructive,
}

fn button(id: impl Into<gpui::ElementId>, label: &str, name: Option<&str>, kind: Kind, enabled: bool, t: &Theme) -> Stateful<Div> {
    let (bg, fg) = match kind {
        Kind::Primary => (t.accent, gpui::white()),
        Kind::Normal => (t.button, t.text),
        Kind::Destructive => (t.button, t.red),
    };
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.))
        .h(px(26.))
        .px(px(11.))
        .rounded(px(6.))
        .bg(bg)
        .text_color(fg)
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .when_some(name, |d, n| d.child(symbol(n, 15., fg)))
        .child(label.to_string())
        .when(!enabled, |d| d.opacity(0.4))
        .when(enabled, |d| d.cursor_pointer().hover(|s| s.opacity(0.85)).active(|s| s.opacity(0.7)))
}

fn icon_button(id: impl Into<gpui::ElementId>, name: &str, t: &Theme) -> Stateful<Div> {
    let hover = t.hover;
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(26.))
        .rounded(px(6.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(symbol(name, 17., t.text2))
}

fn card(t: &Theme) -> Div {
    div().flex().flex_col().rounded(px(10.)).bg(t.card).border_1().border_color(t.card_border)
}

fn section(title: &str, body: impl IntoElement, t: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(t.text2).child(title.to_string()))
        .child(body)
}

/// カード内の 1 行。行の間には区切り線を引く
fn rows(items: Vec<AnyElement>, t: &Theme) -> Div {
    let n = items.len();
    let sep = t.separator;
    card(t).children(items.into_iter().enumerate().map(move |(i, el)| {
        div().px(px(12.)).py(px(10.)).when(i + 1 < n, |d| d.border_b_1().border_color(sep)).child(el)
    }))
}

fn setting_row(tile_el: impl IntoElement, title: &str, detail: impl Into<SharedString>, t: &Theme) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(tile_el)
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(1.))
                .min_w_0()
                .child(div().text_size(px(13.)).child(title.to_string()))
                .child(div().text_size(px(11.)).text_color(t.text2).child(detail.into())),
        )
        .into_any_element()
}

fn kv_row(key: &str, value: impl Into<SharedString>, t: &Theme) -> AnyElement {
    div()
        .flex()
        .gap(px(12.))
        .child(div().w(px(110.)).flex_none().text_size(px(13.)).text_color(t.text2).child(key.to_string()))
        .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(t.text).child(value.into()))
        .into_any_element()
}

/// 行の右端に操作部品(スイッチやボタン)を置く
fn with_control(row: AnyElement, control: impl IntoElement) -> AnyElement {
    div().flex().items_center().gap(px(12.)).child(div().flex_1().min_w_0().child(row)).child(control).into_any_element()
}

fn plain_row(title: &str, detail: impl Into<SharedString>, t: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .min_w_0()
        .child(div().text_size(px(13.)).child(title.to_string()))
        .child(div().text_size(px(11.)).text_color(t.text2).child(detail.into()))
        .into_any_element()
}

fn switch(id: impl Into<ElementId>, on: bool, t: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .w(px(38.))
        .h(px(22.))
        .p(px(2.))
        .rounded_full()
        .cursor_pointer()
        .bg(if on { t.green } else { t.text3 })
        .when(on, |d| d.justify_end())
        .child(div().size(px(18.)).rounded_full().bg(gpui::white()))
}

fn file_name(p: &Path) -> SharedString {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().into()
}

fn level_style(level: Level, t: &Theme) -> (&'static str, Hsla) {
    match level {
        Level::Success => ("check_circle", t.green),
        Level::Info => ("info", t.accent),
        Level::Warn => ("warning", t.orange),
        Level::Error => ("warning", t.red),
    }
}

// ---- 画面 ----

impl MainView {
    fn sidebar(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        // 行の描画で cx を借りる(GIF の読み込み)ので、一覧は先に写しておく
        let items = self.store.read(cx).items.clone();
        let rows: Vec<_> = items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let id = item.entry.bundle_id.clone();
                let selected = self.pane == Pane::Entry(id.clone());
                let needs_attention = item.needs_attention();
                let (selected_bg, hover_bg) = (t.selected, t.hover);
                div()
                    .id(("row", i))
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .px(px(8.))
                    .py(px(5.))
                    .rounded(px(7.))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(selected_bg))
                    .when(!selected, |d| d.hover(move |s| s.bg(hover_bg)))
                    .child(self.picture(&item.entry.icon_path, ("row-icon", i), 28., 7., cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(px(13.)).truncate().child(item.name.clone()))
                            .child(div().text_size(px(11.)).text_color(t.text2).truncate().child(match item.entry.mode {
                                Mode::InPlace => tr("本物に付与", "Applied to the real app"),
                                Mode::Clone => tr("クローン", "Clone"),
                            })),
                    )
                    .when(needs_attention, |d| d.child(symbol("warning", 15., t.orange)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.pane = Pane::Entry(id.clone());
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();

        let store = self.store.read(cx);
        let issues = store.issue_count();
        let (dot, summary) = if store.items.is_empty() {
            (t.text3, tr("まだ登録がありません", "No apps registered yet").to_string())
        } else if issues > 0 {
            (t.orange, l!("要確認 {issues} 件", "Needs attention: {issues}"))
        } else {
            (t.green, l!("{} 件に適用中", "{} icon(s) applied", store.items.len()))
        };
        let last = match (store.settings.auto_sync, store.last_sync) {
            (true, Some(s)) => l!("自動同期: オン · {}", "Auto sync: on · {}", clock(s)),
            (true, None) => tr("自動同期: オン", "Auto sync: on").to_string(),
            (false, _) => tr("自動同期: オフ", "Auto sync: off").to_string(),
        };
        let settings_selected = self.pane == Pane::Settings;
        let add_selected = self.pane == Pane::Add;
        let (selected_bg, hover_bg) = (t.selected, t.hover);

        div()
            .flex()
            .flex_col()
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .bg(t.sidebar)
            .border_r_1()
            .border_color(t.separator)
            .child(
                div()
                    .h(px(TITLEBAR))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_end()
                    .px(px(10.))
                    .child(icon_button("add", "add", t).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.show_add(cx)))),
            )
            .child(
                div()
                    .px(px(18.))
                    .pb(px(4.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text3)
                    .child(tr("アプリ", "Apps")),
            )
            .child(
                div()
                    .id("sidebar-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .gap(px(1.))
                    .px(px(10.))
                    .overflow_y_scroll()
                    .children(rows)
                    .child(
                        div()
                            .id("row-add")
                            .flex()
                            .items_center()
                            .gap(px(9.))
                            .px(px(8.))
                            .py(px(5.))
                            .rounded(px(7.))
                            .cursor_pointer()
                            .text_color(t.text2)
                            .when(add_selected, |d| d.bg(selected_bg))
                            .when(!add_selected, |d| d.hover(move |s| s.bg(hover_bg)))
                            .child(
                                div()
                                    .size(px(28.))
                                    .flex_none()
                                    .rounded(px(7.))
                                    .border_1()
                                    .border_dashed()
                                    .border_color(t.text3)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(symbol("add", 16., t.text2)),
                            )
                            .child(div().text_size(px(13.)).child(tr("アプリを追加", "Add App")))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.show_add(cx))),
                    ),
            )
            .child(
                div().px(px(10.)).pb(px(6.)).child(
                    div()
                        .id("row-settings")
                        .flex()
                        .items_center()
                        .gap(px(9.))
                        .px(px(8.))
                        .py(px(6.))
                        .rounded(px(7.))
                        .cursor_pointer()
                        .when(settings_selected, |d| d.bg(selected_bg))
                        .when(!settings_selected, |d| d.hover(move |s| s.bg(hover_bg)))
                        .child(symbol("settings", 17., t.text2))
                        .child(div().text_size(px(13.)).child(tr("設定", "Settings")))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.show_settings(cx))),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(16.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(t.separator)
                    .child(div().size(px(7.)).flex_none().rounded_full().bg(dot))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).truncate().child(summary))
                            .child(div().text_size(px(10.)).text_color(t.text2).truncate().child(last)),
                    )
                    .child(
                        icon_button("sync", "sync", t)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.store.update(cx, |s, cx| s.sync(true, cx)))),
                    ),
            )
    }

    fn entry_pane(&self, item: &Item, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let id = item.entry.bundle_id.clone();
        let store = self.store.read(cx);
        let (mode_label, mode_color) = match item.entry.mode {
            Mode::InPlace => (tr("本物に付与", "Applied to the real app"), t.green),
            Mode::Clone => (tr("クローン", "Clone"), t.orange),
        };
        let (state_label, state_icon, state_color) = if item.applied && item.dock_stale {
            (tr("Dock の更新が必要", "Dock refresh needed"), "sync", t.orange)
        } else if item.applied {
            (tr("アイコン適用中", "Icon applied"), "check_circle", t.green)
        } else {
            (tr("アイコンが外れています", "The icon was removed"), "warning", t.orange)
        };
        let target = store.target_path(&item.entry);
        // Dock の書き換え中は Dock の再起動を待っているので、ほかの操作で割り込ませない
        let idle = store.busy.is_none();
        let busy_here = store.busy.as_ref().filter(|(b, _)| *b == id).map(|(_, step)| step.clone());
        let busy_for = |job: DockJob| busy_here.clone().filter(|_| store.busy_job == job);
        let (refresh_busy, dock_busy, rejoin_busy) = (busy_for(DockJob::Refresh), busy_for(DockJob::Animation), busy_for(DockJob::Rejoin));
        let split = item.split();

        let accent = t.accent;
        let header = div()
            .flex()
            .items_center()
            .gap(px(20.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        card(t)
                            .id("preview")
                            .size(px(120.))
                            .rounded(px(22.))
                            .items_center()
                            .justify_center()
                            .child(self.picture(&item.entry.icon_path, "detail-icon", 92., 0., cx))
                            .drag_over::<ExternalPaths>(move |s, _, _, _| s.border_color(accent).bg(Theme::tint(accent, 0.08)))
                            .when(idle, |d| {
                                d.cursor_pointer()
                                    .hover(move |s| s.border_color(accent))
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick(PickFor::ChangeImage, cx)))
                            }),
                    )
                    .child(div().text_size(px(10.)).text_color(t.text3).child(tr("クリック・ドロップで変更", "Click or drop to change"))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .min_w_0()
                    .child(div().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).truncate().child(item.name.clone()))
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .child(pill(state_label, state_icon, state_color))
                            .child(pill(mode_label, if item.entry.mode == Mode::InPlace { "check_circle" } else { "info" }, mode_color))
                            .when(split, |d| d.child(pill(tr("タイルが分かれています", "Tiles are split"), "warning", t.orange)))
                            .when(item.tile == Some(TileState::Joined), |d| d.child(pill(tr("Dock で動作中", "Animating in the Dock"), "play_arrow", t.green))),
                    )
                    .child(div().text_size(px(11.)).text_color(t.text2).truncate().child(item.entry.bundle_id.clone()))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .pt(px(4.))
                            .child(button("launch", tr("起動", "Launch"), Some("play_arrow"), Kind::Primary, idle, t).when(idle, |d| {
                                let id = id.clone();
                                d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.store.update(cx, |s, cx| s.launch(&id, cx))))
                            }))
                            .child(button("change-image", tr("画像を変更", "Change Image"), Some("image"), Kind::Normal, idle, t).when(idle, |d| {
                                d.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick(PickFor::ChangeImage, cx)))
                            }))
                            .child(button("reveal", tr("Finder で表示", "Reveal in Finder"), Some("folder_open"), Kind::Normal, true, t).on_click({
                                let id = id.clone();
                                cx.listener(move |this, _: &ClickEvent, _, cx| this.store.update(cx, |s, cx| s.reveal(&id, cx)))
                            }))
                            .child(button("remove", tr("元に戻す", "Restore"), Some("undo"), Kind::Destructive, idle, t).when(idle, |d| {
                                let id = id.clone();
                                d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.store.update(cx, |s, cx| s.remove(&id, cx));
                                }))
                            })),
                    ),
            );

        let issue = item.issue.clone().map(|why| {
            div()
                .flex()
                .gap(px(10.))
                .p(px(12.))
                .rounded(px(10.))
                .bg(Theme::tint(t.orange, 0.10))
                .border_1()
                .border_color(Theme::tint(t.orange, 0.30))
                .child(symbol("warning", 18., t.orange))
                .child(div().flex_1().min_w_0().text_size(px(12.)).child(why))
        });

        let refresh_id = id.clone();
        let refresh_button = button("refresh-dock", if refresh_busy.is_some() { tr("更新中…", "Updating…") } else { tr("Dock を更新", "Refresh Dock") }, Some("sync"), Kind::Normal, idle, t)
            .when(idle, |d| {
                d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.store.update(cx, |s, cx| s.refresh_dock(&refresh_id, cx))
                }))
            });
        let with_button = |row: AnyElement, b| div().flex().items_center().gap(px(12.)).child(div().flex_1().min_w_0().child(row)).child(b).into_any_element();
        let (icon_row, refresh_row) = if item.applied && item.dock_stale {
            let row = setting_row(
                tile("sync", t.orange, 28.),
                tr("Dock がまだ前のアイコンです", "The Dock still shows the old icon"),
                refresh_busy.clone().unwrap_or_else(|| {
                    tr("Dock は前のアイコンを覚えていて、アプリを再起動しても描き直しません。「Dock を更新」で反映されます(Dock が一瞬消えます。アプリは終了しません)", "The Dock remembers the old icon and does not redraw it, even if you restart the app. \"Refresh Dock\" updates it (the Dock disappears briefly; the app does not quit)").into()
                }),
                t,
            );
            (with_button(row, refresh_button), None)
        } else {
            let icon_row = if item.applied {
                setting_row(tile("check_circle", t.green, 28.), tr("アイコンが付いています", "The icon is applied"), tr("Dock・Finder・アプリ切り替えに、選んだ画像が表示されます", "The chosen image is shown in the Dock, Finder, and app switcher"), t)
            } else {
                setting_row(tile("warning", t.orange, 28.), tr("アイコンが外れています", "The icon was removed"), tr("次の自動同期で付け直します。すぐに直すなら左下の同期ボタンを押してください", "It will be reapplied at the next auto sync. To fix it now, press the sync button at the bottom left"), t)
            };
            let row = setting_row(
                tile("settings_backup_restore", t.accent, 28.),
                tr("Dock のアイコンが古いとき", "When the Dock icon is outdated"),
                refresh_busy.clone().unwrap_or_else(|| {
                    tr("Dock が覚えているアイコンを消して、Dock を再起動します。Dock が一瞬消えます。アプリは終了しませんが、起動中の動くタイルは分かれます", "Clears the icon the Dock remembers and restarts the Dock. The Dock disappears briefly. The app does not quit, but running animated tiles will split").into()
                }),
                t,
            );
            (icon_row, Some(with_button(row, refresh_button)))
        };
        let animated = item.entry.animation.is_some();
        let is_gif = item.entry.icon_path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("gif"));
        let anim_id = id.clone();
        let anim_row = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(div().flex_1().min_w_0().child(setting_row(
                tile("play_arrow", if animated { t.green } else { t.accent }, 28.),
                tr("Dock で動かす(実験的)", "Animate in the Dock (experimental)"),
                match (&dock_busy, animated, is_gif) {
                    (Some(step), _, _) => step.clone(),
                    (None, true, _) if item.entry.animation.as_ref().is_some_and(|a| a.direct) => {
                        tr("起動用アプリ(Dock の動くタイル、Spotlight・Raycast に出る同名のアプリ、この画面の「起動」)から開けば、起動中もタイルは 1 つのまま動きます", "Open the app from its launcher (the animated Dock tile, the same-named app in Spotlight or Raycast, or \"Launch\" here) and the tile stays single and animated while it runs").into()
                    }
                    (None, true, _) => tr("このアプリはデータが保護されているため、本物を別に開いています。起動中は本物のタイル(静止画)も並びます", "This app's data is protected, so the real app is opened separately. While it runs, the real tile (still image) appears too").into(),
                    (None, false, true) => tr("本物のタイルは Dock がアイコンを固定するので動かせません。代わりに、動くタイルを持つ起動用アプリを作ります", "The Dock fixes the icon of a real tile, so it cannot be animated. Instead, DockMorph creates a launcher app that owns an animated tile").into(),
                    (None, false, false) => SharedString::from(tr("GIF を選ぶと動きます。今の画像ではそのまま表示されます", "Choose a GIF to animate it. The current image is shown as is")),
                },
                t,
            )))
            .child(
                button("animate", if dock_busy.is_some() { tr("設定中…", "Setting up…") } else if animated { tr("オフにする", "Turn Off") } else { tr("オンにする", "Turn On") }, None, Kind::Normal, idle, t).when(idle, |d| {
                    d.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if animated {
                            this.store.update(cx, |s, cx| s.set_animation(&anim_id, false, false, cx));
                        } else {
                            this.confirm_animation(anim_id.clone(), window, cx);
                        }
                    }))
                }),
            )
            .into_any_element();
        let auto_id = id.clone();
        let auto_row = item.entry.animation.as_ref().filter(|a| a.direct).map(|a| {
            let on = a.auto_rejoin;
            let row = setting_row(
                tile("sync", if on { t.green } else { t.accent }, 28.),
                tr("直接開かれたら知らせる", "Notify when opened directly"),
                if on {
                    tr("本物が起動用アプリを通さずに開かれたら(ファイルのダブルクリックやターミナルからの起動など)、通知とこの画面で知らせます。アプリは終了させません。「まとめ直す」を押すと、終了して起動用アプリから開き直します。このアプリ用のリンク(会議リンクなど)は起動用アプリが受け取るので、はじめから 1 つのタイルで開きます(DockMorph の常駐中だけ働きます)", "If the real app is opened without the launcher app (for example by double-clicking a file or from Terminal), DockMorph notifies you here and in Notification Center. It does not quit the app. \"Rejoin\" quits it and reopens it from the launcher app. Links for this app (such as meeting links) are received by the launcher app, so they open in a single tile from the start (works only while DockMorph is running)")
                } else {
                    tr("オフです。本物が直接開かれても知らせません。分かれたタイルはこの画面とメニューバーの「まとめ直す」で戻せます", "Off. DockMorph does not notify you when the real app is opened directly. \"Rejoin\" in this window or the menu bar restores split tiles")
                },
                t,
            );
            let b = button("auto-rejoin", if on { tr("オフにする", "Turn Off") } else { tr("オンにする", "Turn On") }, None, Kind::Normal, idle, t).when(idle, |d| {
                d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.store.update(cx, |s, cx| s.set_auto_rejoin(&auto_id, !on, cx))))
            });
            with_button(row, b)
        });
        let ax_row = (animated && !self.store.read(cx).ax_trusted).then(|| {
            let row = setting_row(
                tile("warning", t.orange, 28.),
                tr("アクセシビリティの許可がありません", "Accessibility permission is not granted"),
                tr("許可すると、Dock を再起動せずに動くタイルを置く・外すことができ、Dock の右側に残った本物のタイルも自動で消せます。許可がないと Dock を再起動するので起動中のほかの動くタイルが分かれ、残った本物のタイルも消えません", "With permission, DockMorph can place and remove animated tiles without restarting the Dock, and can remove leftover real-app tiles on the right side of the Dock automatically. Without it, the Dock restarts, so other running animated tiles split and leftover real-app tiles stay"),
                t,
            );
            let b = button("ax", tr("許可する", "Allow"), None, Kind::Normal, idle, t).when(idle, |d| {
                d.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.store.update(cx, |s, cx| s.request_accessibility(cx))))
            });
            with_button(row, b)
        });
        let rejoin_id = id.clone();
        let split_row = split.then(|| {
            let row = setting_row(
                tile("warning", t.orange, 28.),
                tr("本物のタイルが別に出ています", "The real app's tile appears separately"),
                rejoin_busy.clone().unwrap_or_else(|| {
                    tr("本物が起動用アプリを通さずに開かれたか、起動したあとに Dock が再起動したため、動かないタイルが Dock の右側に出ています。「まとめ直す」でこのアプリを再起動すると、動くタイル 1 つに戻ります", "The real app was opened without the launcher app, or the Dock restarted after it launched, so a non-animated tile appears on the right side of the Dock. \"Rejoin\" restarts this app and returns to a single animated tile").into()
                }),
                t,
            );
            let b = button("rejoin", if rejoin_busy.is_some() { tr("再起動中…", "Restarting…") } else { tr("まとめ直す", "Rejoin") }, Some("sync"), Kind::Normal, idle, t).when(idle, |d| {
                d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.store.update(cx, |s, cx| s.rejoin(&rejoin_id, cx))))
            });
            with_button(row, b)
        });
        let status = rows(
            vec![
                Some(icon_row),
                refresh_row,
                Some(anim_row),
                auto_row,
                split_row,
                ax_row,
                Some(setting_row(
                    tile("sync", t.accent, 28.),
                    tr("アップデート後も自動で付け直す", "Reapply automatically after updates"),
                    tr("アプリの更新でアイコンが外れても、30 秒以内に付け直します。メニューバーに常駐している間も動きます", "If an app update removes the icon, DockMorph reapplies it within 30 seconds. It works while DockMorph stays in the menu bar"),
                    t,
                )),
            ]
            .into_iter()
            .flatten()
            .collect(),
            t,
        );

        let mut details = vec![
            kv_row(
                tr("方式", "Method"),
                match item.entry.mode {
                    Mode::InPlace => tr("本物のアプリに直接アイコンを付けています。署名はそのまま有効です", "The icon is attached directly to the real app. Its code signature stays valid"),
                    Mode::Clone => tr("本物に書き込めないため、複製(APFS クローン、追加容量なし)にアイコンを付けて、複製を起動します", "The real app is not writable, so the icon is attached to a clone (APFS clone, no extra disk space) and the clone is launched"),
                },
                t,
            ),
            kv_row(tr("元のアプリ", "Original app"), item.entry.source_path.display().to_string(), t),
        ];
        if item.entry.mode == Mode::Clone {
            if let Some(p) = &target {
                details.push(kv_row(tr("起動するアプリ", "App that launches"), p.display().to_string(), t));
            }
        }
        details.push(kv_row(tr("画像", "Image"), item.entry.icon_path.display().to_string(), t));

        div()
            .flex()
            .flex_col()
            .gap(px(22.))
            .child(header)
            .children(issue)
            .child(section(tr("状態", "Status"), status, t))
            .child(section(tr("詳細", "Details"), rows(details, t), t))
            .into_any_element()
    }

    fn settings_pane(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (settings, ax_trusted, data_folder) = {
            let store = self.store.read(cx);
            (store.settings.clone(), store.ax_trusted, store.data_folder())
        };
        let login_on = login::is_enabled();
        let auto_updates = cx.global::<crate::AppState>().updater.as_ref().map(|u| u.automatic_checks());
        let version = updater::app_version().unwrap_or_else(|| tr("開発ビルド", "Development build").to_string());

        let current = lang::resolve(settings.language);
        let language_open = self.language_menu_open;
        let choices = [(Language::English, "English"), (Language::Japanese, "日本語")];
        let current_label = choices.iter().find(|(v, _)| *v == current).map_or("", |(_, name)| *name);
        let (hover, selected_bg) = (t.hover, t.selected);
        let trigger = div()
            .id("lang-dropdown")
            .flex()
            .items_center()
            .justify_between()
            .gap(px(6.))
            .w(px(130.))
            .h(px(26.))
            .pl(px(11.))
            .pr(px(7.))
            .rounded(px(6.))
            .bg(t.button)
            .text_size(px(13.))
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .hover(|s| s.opacity(0.85))
            .child(current_label)
            .child(symbol("expand_more", 16., t.text2))
            // 開いているときの外側クリックはポップアップ側が先に閉じるので、ここは描画時点の状態の反対にする
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.language_menu_open = !language_open;
                cx.notify();
            }));
        let popup = language_open.then(|| {
            deferred(
                div()
                    .absolute()
                    .top(px(30.))
                    .right_0()
                    .w(px(130.))
                    .p(px(4.))
                    .rounded(px(8.))
                    .bg(t.banner)
                    .border_1()
                    .border_color(t.card_border)
                    .occlude()
                    .shadow(vec![BoxShadow {
                        color: Hsla { h: 0., s: 0., l: 0., a: 0.22 },
                        offset: point(px(0.), px(4.)),
                        blur_radius: px(14.),
                        spread_radius: px(0.),
                    }])
                    .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.language_menu_open = false;
                        cx.notify();
                    }))
                    .children(choices.iter().enumerate().map(|(i, (value, name))| {
                        let value = *value;
                        let selected = value == current;
                        div()
                            .id(("lang-option", i))
                            .flex()
                            .items_center()
                            .justify_between()
                            .px(px(8.))
                            .py(px(5.))
                            .rounded(px(5.))
                            .text_size(px(13.))
                            .cursor_pointer()
                            .when(selected, |d| d.bg(selected_bg))
                            .hover(move |s| s.bg(hover))
                            .child(*name)
                            .when(selected, |d| d.child(symbol("check_circle", 14., t.accent)))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.language_menu_open = false;
                                this.store.update(cx, |s, cx| s.set_language(value, cx));
                            }))
                    })),
            )
            .with_priority(1)
        });
        let language = with_control(
            plain_row(
                tr("言語", "Language"),
                tr("既定では、macOS の言語が日本語のときだけ日本語、それ以外は英語で表示します", "By default, DockMorph shows Japanese when your macOS language is Japanese, and English otherwise"),
                t,
            ),
            div().relative().flex_none().child(trigger).children(popup),
        );
        let login_row = match login_on {
            Some(on) => with_control(
                plain_row(tr("ログイン時に起動", "Launch at login"), tr("Mac にログインしたとき、メニューバーに常駐します", "DockMorph stays in the menu bar after you log in"), t),
                switch("sw-login", on, t).on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    if this.store.update(cx, |s, cx| s.toggle_login(cx)) {
                        window.activate_window();
                    }
                    cx.notify();
                })),
            ),
            None => plain_row(tr("ログイン時に起動", "Launch at login"), tr("DockMorph.app として起動したときだけ使えます", "Available only when launched as DockMorph.app"), t),
        };
        let sync_row = with_control(
            plain_row(
                tr("自動で同期する", "Sync automatically"),
                tr(
                    "30 秒ごとに、アップデートで外れたアイコンを付け直し、移動したアプリを探します",
                    "Every 30 seconds, DockMorph reapplies icons removed by app updates and follows apps that moved",
                ),
                t,
            ),
            switch("sw-sync", settings.auto_sync, t).on_click({
                let on = settings.auto_sync;
                cx.listener(move |this, _: &ClickEvent, _, cx| this.store.update(cx, |s, cx| s.set_auto_sync(!on, cx)))
            }),
        );
        let guard_row = with_control(
            plain_row(
                tr("起動中の Antigravity のアイコンを保持する", "Keep the icon of the running Antigravity"),
                tr(
                    "Antigravity が起動中に元のアイコンへ戻すのを抑止します。一時的にローカルのデバッグ接続を使います(Antigravity だけが対象です)。接続を開いている短い間は、同じユーザーのほかのプロセスからも Antigravity に接続できます",
                    "Stops Antigravity from switching back to its original icon while it runs. Uses a temporary local debugging connection (Antigravity only). While it is briefly open, other processes of the same user can also connect to Antigravity",
                ),
                t,
            ),
            switch("sw-guard", settings.keep_running_icon, t).on_click({
                let on = settings.keep_running_icon;
                cx.listener(move |this, _: &ClickEvent, _, cx| this.store.update(cx, |s, cx| s.set_keep_running_icon(!on, cx)))
            }),
        );
        let ax_detail = if ax_trusted {
            tr("許可されています。Dock を再起動せずに動くタイルを置く・外せます", "Allowed. DockMorph can place and remove animated tiles without restarting the Dock")
        } else {
            tr("許可がありません。許可すると、Dock を再起動せずに動くタイルを置く・外せます", "Not allowed. With permission, DockMorph can place and remove animated tiles without restarting the Dock")
        };
        let ax_row = if ax_trusted {
            plain_row(tr("アクセシビリティ", "Accessibility"), ax_detail, t)
        } else {
            with_control(
                plain_row(tr("アクセシビリティ", "Accessibility"), ax_detail, t),
                button("ax-allow", tr("許可する", "Allow"), None, Kind::Normal, true, t)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.store.update(cx, |s, cx| s.request_accessibility(cx)))),
            )
        };
        let updates_enabled = auto_updates.is_some();
        let version_row = with_control(
            plain_row(
                tr("バージョン", "Version"),
                if updates_enabled { version } else { l!("{version}(アップデートの確認は配布版でのみ使えます)", "{version} (update checking is available in release builds only)") },
                t,
            ),
            button("check-updates", tr("今すぐ確認", "Check Now"), None, Kind::Normal, updates_enabled, t).when(updates_enabled, |d| {
                d.on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                    if let Some(updater) = &cx.global::<crate::AppState>().updater {
                        updater.check_for_updates();
                    }
                    cx.activate(true);
                }))
            }),
        );
        let update_rows = match auto_updates {
            Some(on) => vec![
                version_row,
                with_control(
                    plain_row(tr("自動でアップデートを確認する", "Check for updates automatically"), tr("起動中に定期的に確認します", "Checks periodically while DockMorph runs"), t),
                    switch("sw-updates", on, t).on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        if let Some(updater) = &cx.global::<crate::AppState>().updater {
                            updater.set_automatic_checks(!on);
                        }
                        cx.notify();
                    })),
                ),
            ],
            None => vec![version_row],
        };
        let data_row = with_control(
            plain_row(tr("データの場所", "Data folder"), data_folder, t),
            button("reveal-data", tr("Finder で表示", "Reveal in Finder"), Some("folder_open"), Kind::Normal, true, t)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.store.update(cx, |s, cx| s.reveal_data_folder(cx)))),
        );
        let idle = self.store.read(cx).busy.is_none();
        let restore_row = with_control(
            plain_row(
                tr("すべて元に戻して終了", "Restore Everything and Quit"),
                tr("アンインストールの前に使います。すべてのアプリのアイコン・Dock の動くタイル・ログイン項目を元に戻してから終了します", "Use this before uninstalling. It restores all app icons, animated Dock tiles, and Login Items, then quits"),
                t,
            ),
            button("restore-all", tr("元に戻して終了…", "Restore and Quit…"), Some("undo"), Kind::Destructive, idle, t).when(idle, |d| {
                d.on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.confirm_restore_all(window, cx)))
            }),
        );

        div()
            .flex()
            .flex_col()
            .gap(px(22.))
            .child(section(tr("一般", "General"), rows(vec![language, login_row], t), t))
            .child(section(tr("同期", "Sync"), rows(vec![sync_row, guard_row], t), t))
            .child(section(tr("許可", "Permissions"), rows(vec![ax_row], t), t))
            .child(section(tr("アップデート", "Updates"), rows(update_rows, t), t))
            .child(section(tr("データ", "Data"), rows(vec![data_row, restore_row], t), t))
            .into_any_element()
    }

    fn slot(&self, for_app: bool, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let (path, label, empty_icon, tint) = if for_app {
            (self.pending_app.clone(), tr("アプリ", "App"), "apps", t.accent)
        } else {
            (self.pending_image.clone(), tr("新しいアイコン", "New icon"), "image", t.green)
        };
        let filled = path.is_some();
        let preview: AnyElement = match (&path, for_app) {
            (Some(p), false) => self.picture(p, "pending-icon", 72., 16., cx),
            (Some(_), true) if self.pending_app_icon.is_some() => {
                let icon = self.pending_app_icon.clone().expect("checked with is_some");
                img(icon).id("pending-app-icon").size(px(72.)).flex_none().into_any_element()
            }
            _ => tile(empty_icon, tint, 56.).into_any_element(),
        };
        let accent = t.accent;
        card(t)
            .id(if for_app { "slot-app" } else { "slot-image" })
            .flex_1()
            .min_w_0()
            .h(px(190.))
            .items_center()
            .justify_center()
            .gap(px(8.))
            .p(px(16.))
            .cursor_pointer()
            .when(!filled, |d| d.border_dashed().border_color(t.text3))
            .hover(move |s| s.border_color(accent))
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.border_color(accent).bg(Theme::tint(accent, 0.08)))
            .child(preview)
            .child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.text2).child(label))
            .child(
                div()
                    .text_size(px(13.))
                    .max_w_full()
                    .truncate()
                    .text_color(if filled { t.text } else { t.text3 })
                    .child(path.as_deref().map(file_name).unwrap_or_else(|| tr("クリックして選ぶ・ドロップ", "Click to choose, or drop").into())),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.pick(if for_app { PickFor::App } else { PickFor::NewImage }, cx)
            }))
    }

    fn add_pane(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let can_add = self.pending_app.is_some() && self.pending_image.is_some();
        div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).child(tr("アプリのアイコンを変える", "Change an app's icon")))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(t.text2)
                            .child(tr("アプリと画像を選ぶか、このウィンドウにドロップしてください。Dock は純正のまま、起動中もアイコンが変わります。", "Choose an app and an image, or drop them on this window. The Dock stays stock, and the icon changes while the app runs.")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(self.slot(true, t, cx))
                    .child(symbol("play_arrow", 22., t.text3))
                    .child(self.slot(false, t, cx)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(8.))
                            .items_start()
                            .child(symbol("info", 16., t.text2))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(12.))
                                    .text_color(t.text2)
                                    .child(tr("本物のアプリに直接アイコンを付けます。書き込めないアプリだけ、複製を作る方式に切り替えてお知らせします。元に戻すといつでも元通りになります。", "The icon is attached directly to the real app. Only if the app is not writable, DockMorph switches to a clone and tells you. You can always restore the original.")),
                            ),
                    )
                    .child(
                        button("do-add", tr("アイコンを変える", "Change Icon"), Some("check_circle"), Kind::Primary, can_add, t)
                            .when(can_add, |d| d.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.add(cx)))),
                    ),
            )
            .into_any_element()
    }

    fn banner(&self, t: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let banner = self.store.read(cx).banner.clone()?;
        let (name, tint) = level_style(banner.level, t);
        Some(
            div().absolute().top(px(10.)).left_0().right_0().flex().justify_center().child(
                div()
                    .id("banner")
                    .occlude()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .max_w(px(540.))
                    .mx(px(16.))
                    .px(px(14.))
                    .py(px(10.))
                    .rounded(px(14.))
                    .bg(t.banner)
                    .border_1()
                    .border_color(t.card_border)
                    .shadow(vec![BoxShadow {
                        color: Hsla { h: 0., s: 0., l: 0., a: 0.22 },
                        offset: point(px(0.), px(4.)),
                        blur_radius: px(14.),
                        spread_radius: px(0.),
                    }])
                    .child(symbol(name, 18., tint))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(banner.title.clone()))
                            .children(
                                banner.detail.clone().map(|d| div().text_size(px(11.)).text_color(t.text2).child(d)),
                            )
                            .children(banner.action.clone().map(|action| {
                                let idle = self.store.read(cx).busy.is_none();
                                let label = match &action {
                                    BannerAction::Rejoin(_) => tr("まとめ直す", "Rejoin"),
                                };
                                div().pt(px(6.)).child(button("banner-action", label, Some("sync"), Kind::Normal, idle, t).when(idle, |d| {
                                    d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        let action = action.clone();
                                        this.store.update(cx, |s, cx| match action {
                                            BannerAction::Rejoin(id) => s.rejoin(&id, cx),
                                        })
                                    }))
                                }))
                            })),
                    )
                    .child(
                        icon_button("banner-close", "close", t)
                            .size(px(20.))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.store.update(cx, |s, cx| s.dismiss_banner(cx)))),
                    ),
            ),
        )
    }
}

impl Render for MainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::for_appearance(window.appearance());
        let item = match &self.pane {
            Pane::Entry(id) => self.store.read(cx).item(id).cloned(),
            Pane::Add | Pane::Settings => None,
        };
        let title: SharedString = match (&item, &self.pane) {
            (Some(i), _) => i.name.clone(),
            (None, Pane::Settings) => tr("設定", "Settings").into(),
            (None, _) => tr("アプリを追加", "Add App").into(),
        };
        let body = match (&item, &self.pane) {
            (Some(i), _) => self.entry_pane(i, &t, cx),
            (None, Pane::Settings) => self.settings_pane(&t, cx),
            (None, _) => self.add_pane(&t, cx),
        };
        let accent = t.accent;

        let root = div()
            .track_focus(&self.focus)
            .key_context("MainView")
            .on_action(cx.listener(|_, _: &CloseWindow, window, _| window.remove_window()))
            .on_action(cx.listener(|this, _: &SyncNow, _, cx| this.store.update(cx, |s, cx| s.sync(true, cx))))
            .on_action(cx.listener(|this, _: &AddApp, _, cx| this.show_add(cx)))
            .relative()
            .flex()
            .size_full()
            .bg(t.content)
            .text_color(t.text)
            .text_size(px(13.))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.drop_paths(paths.paths(), cx)))
            .child(self.sidebar(&t, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        div()
                            .h(px(TITLEBAR))
                            .flex_none()
                            .flex()
                            .items_center()
                            .px(px(20.))
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .id("content")
                            .flex_1()
                            .overflow_y_scroll()
                            .child(div().px(px(28.)).pt(px(8.)).pb(px(28.)).max_w(px(860.)).child(body)),
                    ),
            )
            // ファイルを持ってきている間だけ、ウィンドウ全体を受け口として縁取る
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(10.))
                    .border_2()
                    .border_color(gpui::transparent_black())
                    .drag_over::<ExternalPaths>(move |s, _, _, _| s.border_color(accent).bg(Theme::tint(accent, 0.06))),
            )
            .children(self.banner(&t, cx));
        self.gifs.finish_frame(window, cx);
        root
    }
}

pub fn min_size() -> gpui::Size<Pixels> {
    gpui::size(px(720.), px(480.))
}
