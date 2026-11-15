use crate::assets::icon;
use crate::gif::Gifs;
use crate::store::{clock, DockJob, Item, Level, Store};
use crate::theme::Theme;
use crate::{AddApp, CloseWindow, SyncNow};
use dockmorph_core::{DockMorph, Mode, TileState};
use std::sync::Arc;
use gpui::{
    div, img, linear_color_stop, linear_gradient, prelude::*, px, svg, AnyElement, BoxShadow, ClickEvent, Context, Div,
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
        MainView { store, pane, pending_app: None, pending_app_icon: None, pending_image: None, picking: false, gifs: Gifs::new(), focus, _subscriptions: subscriptions }
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
                    self.notify_error("アプリのアイコンを表示できません", format!("{e:#}"), cx);
                    None
                }
            };
        } else {
            self.notify_error("アプリ(.app)ではありません", path.display().to_string(), cx);
        }
    }

    fn check_image(&self, path: &Path, cx: &mut Context<Self>) -> bool {
        let ok = is_image(path);
        if !ok {
            self.notify_error("対応していない画像形式です", format!("png / jpg / webp / gif を選んでください: {}", path.display()), cx);
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
            return self.notify_error("Dock の更新中です", "終わってから画像を変えてください".into(), cx);
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
            crate::panel::pick_file("アプリを選択", Path::new("/Applications"))
        } else {
            cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("画像を選択".into()) })
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
                    Ok(Err(e)) => this.notify_error("ファイルを選べません", format!("{e:#}"), cx),
                    Err(e) => this.notify_error("ファイル選択が中断されました", e.to_string(), cx),
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
            "Dock で画像を動かしますか?(実験的)",
            Some(
                "動くタイルを表示する小さな起動用アプリを作ります。起動用アプリから開けば、起動中もタイルは 1 つのまま動きます。\n\n\
                 「Dock に置く」: 起動用アプリを Dock にピン留めします。このアプリがピン留めされていれば、その位置を置き換えます。\
                 設定のとき Dock が一瞬消えて再起動します(変更前の Dock の設定は保存します)。起動中のほかの動くタイルは分かれます\n\n\
                 「Dock に置かない」: Dock の並びは変えません。Spotlight・Raycast・この画面から起動用アプリを開くと、起動中だけ Dock の右側で動きます\n\n\
                 ・本物を直接開いたときや、起動後に Dock が再起動したときは、本物のタイル(静止画)が別に出ます。この画面の「まとめ直す」で戻せます\n\
                 ・データが macOS に保護されているアプリ(Firefox など)は、本物を別に開く方式になり、起動中はタイルが 2 つ並びます",
            ),
            &[PromptButton::ok("Dock に置く"), PromptButton::new("Dock に置かない"), PromptButton::cancel("キャンセル")],
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
        if let Some(id) = self.store.update(cx, |s, cx| s.add(&app, &image, cx)) {
            self.pending_app = None;
            self.pending_app_icon = None;
            self.pending_image = None;
            self.pane = Pane::Entry(id);
        }
        cx.notify();
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
                                Mode::InPlace => "本物に付与",
                                Mode::Clone => "クローン",
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
            (t.text3, "まだ登録がありません".to_string())
        } else if issues > 0 {
            (t.orange, format!("要確認 {issues} 件"))
        } else {
            (t.green, format!("{} 件に適用中", store.items.len()))
        };
        let last = store.last_sync.map(|s| format!("自動同期: オン · {}", clock(s))).unwrap_or_else(|| "自動同期: オン".into());
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
                    .child("アプリ"),
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
                            .child(div().text_size(px(13.)).child("アプリを追加"))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.show_add(cx))),
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
            Mode::InPlace => ("本物に付与", t.green),
            Mode::Clone => ("クローン", t.orange),
        };
        let (state_label, state_icon, state_color) = if item.applied && item.dock_stale {
            ("Dock の更新が必要", "sync", t.orange)
        } else if item.applied {
            ("アイコン適用中", "check_circle", t.green)
        } else {
            ("アイコンが外れています", "warning", t.orange)
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
                    .child(div().text_size(px(10.)).text_color(t.text3).child("クリック・ドロップで変更")),
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
                            .when(split, |d| d.child(pill("タイルが分かれています", "warning", t.orange)))
                            .when(item.tile == Some(TileState::Joined), |d| d.child(pill("Dock で動作中", "play_arrow", t.green))),
                    )
                    .child(div().text_size(px(11.)).text_color(t.text2).truncate().child(item.entry.bundle_id.clone()))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .pt(px(4.))
                            .child(button("launch", "起動", Some("play_arrow"), Kind::Primary, idle, t).when(idle, |d| {
                                let id = id.clone();
                                d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.store.update(cx, |s, cx| s.launch(&id, cx))))
                            }))
                            .child(button("change-image", "画像を変更", Some("image"), Kind::Normal, idle, t).when(idle, |d| {
                                d.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick(PickFor::ChangeImage, cx)))
                            }))
                            .child(button("reveal", "Finder で表示", Some("folder_open"), Kind::Normal, true, t).on_click({
                                let id = id.clone();
                                cx.listener(move |this, _: &ClickEvent, _, cx| this.store.update(cx, |s, cx| s.reveal(&id, cx)))
                            }))
                            .child(button("remove", "元に戻す", Some("undo"), Kind::Destructive, idle, t).when(idle, |d| {
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
        let refresh_button = button("refresh-dock", if refresh_busy.is_some() { "更新中…" } else { "Dock を更新" }, Some("sync"), Kind::Normal, idle, t)
            .when(idle, |d| {
                d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.store.update(cx, |s, cx| s.refresh_dock(&refresh_id, cx))
                }))
            });
        let with_button = |row: AnyElement, b| div().flex().items_center().gap(px(12.)).child(div().flex_1().min_w_0().child(row)).child(b).into_any_element();
        let (icon_row, refresh_row) = if item.applied && item.dock_stale {
            let row = setting_row(
                tile("sync", t.orange, 28.),
                "Dock がまだ前のアイコンです",
                refresh_busy.clone().unwrap_or_else(|| {
                    "Dock は前のアイコンを覚えていて、アプリを再起動しても描き直しません。「Dock を更新」で反映されます(Dock が一瞬消えます。アプリは終了しません)".into()
                }),
                t,
            );
            (with_button(row, refresh_button), None)
        } else {
            let icon_row = if item.applied {
                setting_row(tile("check_circle", t.green, 28.), "アイコンが付いています", "Dock・Finder・アプリ切り替えに、選んだ画像が表示されます", t)
            } else {
                setting_row(tile("warning", t.orange, 28.), "アイコンが外れています", "次の自動同期で付け直します。すぐに直すなら左下の同期ボタンを押してください", t)
            };
            let row = setting_row(
                tile("settings_backup_restore", t.accent, 28.),
                "Dock のアイコンが古いとき",
                refresh_busy.clone().unwrap_or_else(|| {
                    "Dock が覚えているアイコンを消して、Dock を再起動します。Dock が一瞬消えます。アプリは終了しませんが、起動中の動くタイルは分かれます".into()
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
                "Dock で動かす(実験的)",
                match (&dock_busy, animated, is_gif) {
                    (Some(step), _, _) => step.clone(),
                    (None, true, _) if item.entry.animation.as_ref().is_some_and(|a| a.direct) => {
                        "起動用アプリ(Dock の動くタイル、Spotlight・Raycast に出る同名のアプリ、この画面の「起動」)から開けば、起動中もタイルは 1 つのまま動きます".into()
                    }
                    (None, true, _) => "このアプリはデータが保護されているため、本物を別に開いています。起動中は本物のタイル(静止画)も並びます".into(),
                    (None, false, true) => "本物のタイルは Dock がアイコンを固定するので動かせません。代わりに、動くタイルを持つ起動用アプリを作ります".into(),
                    (None, false, false) => SharedString::from("GIF を選ぶと動きます。今の画像ではそのまま表示されます"),
                },
                t,
            )))
            .child(
                button("animate", if dock_busy.is_some() { "設定中…" } else if animated { "オフにする" } else { "オンにする" }, None, Kind::Normal, idle, t).when(idle, |d| {
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
                "直接開かれたら自動でまとめ直す",
                if on {
                    "本物が直接開かれたら、起動した直後に自動で終了して、起動用アプリから開き直します。このアプリ用のリンク(会議リンクなど)は起動用アプリが受け取るので、そのまま 1 つのタイルで開きます。ファイルから開いた場合は、そのファイルは引き継がれません(DockMorph の常駐中だけ働きます)"
                } else {
                    "オフです。本物が直接開かれると、動かないタイルが出ます。「まとめ直す」で戻せます"
                },
                t,
            );
            let b = button("auto-rejoin", if on { "オフにする" } else { "オンにする" }, None, Kind::Normal, idle, t).when(idle, |d| {
                d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.store.update(cx, |s, cx| s.set_auto_rejoin(&auto_id, !on, cx))))
            });
            with_button(row, b)
        });
        let ax_row = (animated && !self.store.read(cx).ax_trusted).then(|| {
            let row = setting_row(
                tile("warning", t.orange, 28.),
                "アクセシビリティの許可がありません",
                "許可すると、オフにしたときに Dock を再起動せずに起動用アプリのタイルを外せます。許可がないと Dock を再起動するので、起動中のほかの動くタイルが分かれることがあります",
                t,
            );
            let b = button("ax", "許可する", None, Kind::Normal, idle, t).when(idle, |d| {
                d.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.store.update(cx, |s, cx| s.request_accessibility(cx))))
            });
            with_button(row, b)
        });
        let rejoin_id = id.clone();
        let split_row = split.then(|| {
            let row = setting_row(
                tile("warning", t.orange, 28.),
                "本物のタイルが別に出ています",
                rejoin_busy.clone().unwrap_or_else(|| {
                    "本物が起動用アプリを通さずに開かれたか、起動したあとに Dock が再起動したため、動かないタイルが Dock の右側に出ています。「まとめ直す」でこのアプリを再起動すると、動くタイル 1 つに戻ります".into()
                }),
                t,
            );
            let b = button("rejoin", if rejoin_busy.is_some() { "再起動中…" } else { "まとめ直す" }, Some("sync"), Kind::Normal, idle, t).when(idle, |d| {
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
                    "アップデート後も自動で付け直す",
                    "アプリの更新でアイコンが外れても、30 秒以内に付け直します。メニューバーに常駐している間も動きます",
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
                "方式",
                match item.entry.mode {
                    Mode::InPlace => "本物のアプリに直接アイコンを付けています。署名はそのまま有効です",
                    Mode::Clone => "本物に書き込めないため、複製(APFS クローン、追加容量なし)にアイコンを付けて、複製を起動します",
                },
                t,
            ),
            kv_row("元のアプリ", item.entry.source_path.display().to_string(), t),
        ];
        if item.entry.mode == Mode::Clone {
            if let Some(p) = &target {
                details.push(kv_row("起動するアプリ", p.display().to_string(), t));
            }
        }
        details.push(kv_row("画像", item.entry.icon_path.display().to_string(), t));

        div()
            .flex()
            .flex_col()
            .gap(px(22.))
            .child(header)
            .children(issue)
            .child(section("状態", status, t))
            .child(section("詳細", rows(details, t), t))
            .into_any_element()
    }

    fn slot(&self, for_app: bool, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let (path, label, empty_icon, tint) = if for_app {
            (self.pending_app.clone(), "アプリ", "apps", t.accent)
        } else {
            (self.pending_image.clone(), "新しいアイコン", "image", t.green)
        };
        let filled = path.is_some();
        let preview: AnyElement = match (&path, for_app) {
            (Some(p), false) => self.picture(p, "pending-icon", 72., 16., cx),
            (Some(_), true) if self.pending_app_icon.is_some() => {
                let icon = self.pending_app_icon.clone().expect("is_some で確認済み");
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
                    .child(path.as_deref().map(file_name).unwrap_or_else(|| "クリックして選ぶ・ドロップ".into())),
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
                    .child(div().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).child("アプリのアイコンを変える"))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(t.text2)
                            .child("アプリと画像を選ぶか、このウィンドウにドロップしてください。Dock は純正のまま、起動中もアイコンが変わります。"),
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
                                    .child("本物のアプリに直接アイコンを付けます。書き込めないアプリだけ、複製を作る方式に切り替えてお知らせします。元に戻すといつでも元通りになります。"),
                            ),
                    )
                    .child(
                        button("do-add", "アイコンを変える", Some("check_circle"), Kind::Primary, can_add, t)
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
                            ),
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
            Pane::Add => None,
        };
        let title: SharedString = match &item {
            Some(i) => i.name.clone(),
            None => "アプリを追加".into(),
        };
        let body = match &item {
            Some(i) => self.entry_pane(i, &t, cx),
            None => self.add_pane(&t, cx),
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
