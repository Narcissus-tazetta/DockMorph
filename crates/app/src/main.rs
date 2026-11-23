mod assets;
mod gif;
mod icon_guard;
mod instance;
mod login;
mod menubar;
mod notify;
mod panel;
mod store;
mod theme;
mod ui;
mod updater;

use dockmorph_core::{lang, tr, DockMorph, Settings};
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
    updater: Option<updater::Updater>,
    /// アプリのメニュー(cx.set_menus)を作った言語。変わったら作り直す
    menu_japanese: bool,
    _open_listener: objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2::runtime::NSObjectProtocol>>,
}

impl Global for AppState {}

fn set_dock_icon_visible(visible: bool) {
    let mtm = MainThreadMarker::new().expect("gpui callbacks run on the main thread");
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
        .expect("Cannot open the window");
    cx.global_mut::<AppState>().window = Some(handle);
    cx.activate(true);
}

fn set_app_menus(cx: &mut App) {
    cx.set_menus(vec![
        Menu { name: "DockMorph".into(), items: vec![MenuItem::action(tr("DockMorph を終了", "Quit DockMorph"), Quit)] },
        Menu {
            name: tr("ファイル", "File").into(),
            items: vec![
                MenuItem::action(tr("アプリを追加", "Add App"), AddApp),
                MenuItem::action(tr("今すぐ同期", "Sync Now"), SyncNow),
                MenuItem::separator(),
                MenuItem::action(tr("ウィンドウを閉じる", "Close Window"), CloseWindow),
            ],
        },
    ]);
    cx.global_mut::<AppState>().menu_japanese = lang::is_japanese();
}

fn rebuild_menu(cx: &mut App) {
    if cx.global::<AppState>().menu_japanese != lang::is_japanese() {
        set_app_menus(cx);
    }
    let state = cx.global::<AppState>();
    state.menubar.rebuild(state.store.read(cx), login::is_enabled(), state.updater.is_some());
}

fn handle(command: Command, cx: &mut App) {
    let store = cx.global::<AppState>().store.clone();
    match command {
        Command::Open => open_window(cx),
        Command::Sync => store.update(cx, |s, cx| s.sync(true, cx)),
        Command::Launch(id) => store.update(cx, |s, cx| s.launch(&id, cx)),
        Command::Rejoin(id) => store.update(cx, |s, cx| s.rejoin(&id, cx)),
        Command::NotifyFailed(reason) => store.update(cx, |s, cx| s.notification_failed(reason, cx)),
        Command::ToggleLogin => {
            if store.update(cx, |s, cx| s.toggle_login(cx)) {
                open_window(cx);
            }
            rebuild_menu(cx);
        }
        Command::CheckUpdates => {
            if let Some(updater) = &cx.global::<AppState>().updater {
                updater.check_for_updates();
                cx.activate(true);
            }
        }
        Command::Quit => cx.quit(),
    }
}

fn main() -> anyhow::Result<()> {
    let dm = DockMorph::new()?;
    let Some(_instance) = dm.try_instance_lock()? else {
        instance::ask_running_to_open();
        return Ok(());
    };
    // 設定が壊れていても起動はする。画面が出てから理由を伝える
    let (settings, settings_error) = match dm.settings() {
        Ok(settings) => (settings, None),
        Err(e) => (Settings::default(), Some(format!("{e:#}"))),
    };
    lang::set(settings.language);
    let app = Application::new().with_assets(assets::Assets);
    app.on_reopen(open_window);
    app.run(move |cx: &mut App| {
        let mtm = MainThreadMarker::new().expect("gpui callbacks run on the main thread");
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let notifier = notify::Notifier::start(tx.clone());
        let store = cx.new(|cx| Store::new(dm, settings, notifier, cx));
        let (updater, updater_error) = match updater::Updater::start() {
            Ok(updater) => (updater, None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        let open_listener = instance::listen(tx.clone());
        cx.set_global(AppState {
            store: store.clone(),
            window: None,
            menubar: MenuBar::new(mtm, tx),
            updater,
            menu_japanese: lang::is_japanese(),
            _open_listener: open_listener,
        });
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
        set_app_menus(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.global_mut::<AppState>().window = None;
                set_dock_icon_visible(false);
            }
        })
        .detach();

        if let Some(detail) = settings_error {
            store.update(cx, |s, cx| s.notify(Level::Error, tr("設定を読み込めません", "Cannot load the settings"), Some(detail), cx));
        }
        if let Some(detail) = updater_error {
            store.update(cx, |s, cx| s.notify(Level::Error, tr("アップデートの確認を使えません", "Update checking is unavailable"), Some(detail), cx));
        }
        open_window(cx);
    });
    Ok(())
}
