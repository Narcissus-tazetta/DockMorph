mod assets;
mod gif;
mod login;
mod menubar;
mod store;
mod theme;
mod ui;

use dockmorph_core::DockMorph;
use futures::StreamExt;
use gpui::{
    actions, point, px, size, App, AppContext, Application, Bounds, Entity, Global, KeyBinding, Menu, MenuItem,
    TitlebarOptions, WindowBounds, WindowHandle, WindowOptions,
};
use menubar::{Command, MenuBar};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use store::{Level, Store};
use ui::MainView;

actions!(dockmorph, [Quit, CloseWindow, SyncNow, AddApp, OpenWindow]);

struct AppState {
    store: Entity<Store>,
    window: Option<WindowHandle<MainView>>,
    menubar: MenuBar,
}

impl Global for AppState {}

fn set_dock_icon_visible(visible: bool) {
    let mtm = MainThreadMarker::new().expect("gpui のコールバックはメインスレッドで呼ばれる");
    let policy = if visible { NSApplicationActivationPolicy::Regular } else { NSApplicationActivationPolicy::Accessory };
    NSApplication::sharedApplication(mtm).setActivationPolicy(policy);
}

/// ウィンドウがある間だけ Dock とアプリ切り替えに出る。閉じたらメニューバーだけに残る
fn open_window(cx: &mut App) {
    if let Some(handle) = cx.global::<AppState>().window {
        if handle.update(cx, |_, window, _| window.activate_window()).is_ok() {
            cx.activate(true);
            return;
        }
    }
    set_dock_icon_visible(true);
    let store = cx.global::<AppState>().store.clone();
    let bounds = Bounds::centered(None, size(px(900.), px(600.)), cx);
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("DockMorph".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(16.), px(16.))),
                }),
                window_min_size: Some(ui::min_size()),
                ..Default::default()
            },
            move |window, cx| cx.new(|cx| MainView::new(store, window, cx)),
        )
        .expect("ウィンドウを開けません");
    cx.global_mut::<AppState>().window = Some(handle);
    cx.activate(true);
}

fn rebuild_menu(cx: &mut App) {
    let state = cx.global::<AppState>();
    state.menubar.rebuild(state.store.read(cx), login::is_enabled());
}

fn handle(command: Command, cx: &mut App) {
    let store = cx.global::<AppState>().store.clone();
    match command {
        Command::Open => open_window(cx),
        Command::Sync => store.update(cx, |s, cx| s.sync(true, cx)),
        Command::Launch(id) => store.update(cx, |s, cx| s.launch(&id, cx)),
        Command::ToggleLogin => {
            let (level, title, detail) = match login::toggle() {
                Ok(login::Outcome::Enabled) => (Level::Success, "ログイン時に起動します", None),
                Ok(login::Outcome::Disabled) => (Level::Info, "ログイン時に起動しません", None),
                Ok(login::Outcome::NeedsApproval) => (
                    Level::Warn,
                    "ログイン項目の許可が必要です",
                    Some("開いたシステム設定の「ログイン項目」で DockMorph をオンにしてください".to_string()),
                ),
                Err(e) => (Level::Error, "ログイン項目を変更できません", Some(format!("{e:#}"))),
            };
            store.update(cx, |s, cx| s.notify(level, title, detail, cx));
            rebuild_menu(cx);
            // 結果はバナーでしか伝えられないので、ウィンドウが無ければ開く
            if level != Level::Success && level != Level::Info {
                open_window(cx);
            }
        }
        Command::Quit => cx.quit(),
    }
}

fn main() -> anyhow::Result<()> {
    let dm = DockMorph::new()?;
    let app = Application::new().with_assets(assets::Assets);
    app.on_reopen(open_window);
    app.run(move |cx: &mut App| {
        let mtm = MainThreadMarker::new().expect("gpui のコールバックはメインスレッドで呼ばれる");
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let store = cx.new(|cx| Store::new(dm, cx));
        cx.set_global(AppState { store: store.clone(), window: None, menubar: MenuBar::new(mtm, tx) });
        rebuild_menu(cx);
        cx.observe(&store, |_, cx| rebuild_menu(cx)).detach();
        cx.spawn(async move |cx| {
            while let Some(command) = rx.next().await {
                if cx.update(|cx| handle(command, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &OpenWindow, cx| open_window(cx));
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-w", CloseWindow, None),
            KeyBinding::new("cmd-r", SyncNow, None),
            KeyBinding::new("cmd-n", AddApp, None),
        ]);
        cx.set_menus(vec![
            Menu {
                name: "DockMorph".into(),
                items: vec![MenuItem::action("DockMorph を終了", Quit)],
            },
            Menu {
                name: "ファイル".into(),
                items: vec![
                    MenuItem::action("アプリを追加", AddApp),
                    MenuItem::action("今すぐ同期", SyncNow),
                    MenuItem::separator(),
                    MenuItem::action("ウィンドウを閉じる", CloseWindow),
                ],
            },
        ]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.global_mut::<AppState>().window = None;
                set_dock_icon_visible(false);
            }
        })
        .detach();

        open_window(cx);
    });
    Ok(())
}
