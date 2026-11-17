use dockmorph_core::{l, tr};
use crate::assets::MENUBAR_ICON;
use crate::store::Store;
use futures::channel::mpsc::UnboundedSender;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, msg_send, sel, AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSControlStateValueOff, NSControlStateValueOn, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{NSData, NSSize, NSString};

pub enum Command {
    Open,
    Sync,
    Launch(String),
    ToggleLogin,
    CheckUpdates,
    Quit,
}

const TAG_OPEN: isize = 1;
const TAG_SYNC: isize = 2;
const TAG_LAUNCH: isize = 3;
const TAG_LOGIN: isize = 4;
const TAG_QUIT: isize = 5;
const TAG_UPDATE: isize = 6;

define_class!(
    // NSMenuItem の target は弱参照なので、MenuBar がこのオブジェクトを持ち続ける
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = UnboundedSender<Command>]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(menuAction:))]
        fn menu_action(&self, sender: &NSMenuItem) {
            let command = match sender.tag() {
                TAG_OPEN => Command::Open,
                TAG_SYNC => Command::Sync,
                TAG_LOGIN => Command::ToggleLogin,
                TAG_UPDATE => Command::CheckUpdates,
                TAG_QUIT => Command::Quit,
                TAG_LAUNCH => {
                    let Some(id) = sender.representedObject().and_then(|o| o.downcast::<NSString>().ok()) else {
                        eprintln!("Menu item has no bundle ID: {}", sender.title());
                        return;
                    };
                    Command::Launch(id.to_string())
                }
                tag => {
                    eprintln!("Unknown menu item: tag={tag}");
                    return;
                }
            };
            if let Err(e) = self.ivars().unbounded_send(command) {
                eprintln!("Cannot pass the menu action to the app: {e}");
            }
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker, tx: UnboundedSender<Command>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(tx);
        unsafe { msg_send![super(this), init] }
    }
}

pub struct MenuBar {
    _item: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
    target: Retained<MenuTarget>,
    mtm: MainThreadMarker,
}

impl MenuBar {
    pub fn new(mtm: MainThreadMarker, tx: UnboundedSender<Command>) -> Self {
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = item.button(mtm) {
            let icon = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(MENUBAR_ICON))
                .expect("Cannot read the embedded menu bar icon");
            // テンプレート画像にすると、メニューバーの明暗や選択状態に合わせて OS が色を付ける
            icon.setTemplate(true);
            icon.setSize(NSSize::new(18., 18.));
            button.setImage(Some(&icon));
            button.setToolTip(Some(&NSString::from_str("DockMorph")));
        }
        let menu = NSMenu::new(mtm);
        item.setMenu(Some(&menu));
        MenuBar { _item: item, menu, target: MenuTarget::new(mtm, tx), mtm }
    }

    fn add(&self, title: &str, tag: isize, key: &str) -> Retained<NSMenuItem> {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm),
                &NSString::from_str(title),
                Some(sel!(menuAction:)),
                &NSString::from_str(key),
            )
        };
        item.setTag(tag);
        unsafe { item.setTarget(Some(&self.target)) };
        self.menu.addItem(&item);
        item
    }

    /// action を持たない項目は NSMenu の自動有効化で灰色になる。状態の表示に使う
    fn add_label(&self, title: &str) {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm),
                &NSString::from_str(title),
                None,
                &NSString::new(),
            )
        };
        self.menu.addItem(&item);
    }

    fn separator(&self) {
        self.menu.addItem(&NSMenuItem::separatorItem(self.mtm));
    }

    /// 登録や同期結果が変わるたびに作り直す。項目は数件なので差分更新より単純さを取る
    pub fn rebuild(&self, store: &Store, login_item: Option<bool>, updates: bool) {
        self.menu.removeAllItems();
        let issues = store.issue_count();
        self.add_label(&if store.items.is_empty() {
            tr("まだ登録がありません", "No apps registered yet").to_string()
        } else if issues > 0 {
            l!("要確認 {issues} 件", "Needs attention: {issues}")
        } else {
            l!("{} 件のアイコンを適用中", "{} icon(s) applied", store.items.len())
        });
        if !store.items.is_empty() {
            self.separator();
            for item in &store.items {
                let mark = if item.needs_attention() { " ⚠︎" } else { "" };
                let menu_item = self.add(&format!("{}{mark}", item.name), TAG_LAUNCH, "");
                let id = NSString::from_str(&item.entry.bundle_id);
                unsafe { menu_item.setRepresentedObject(Some(&id as &AnyObject)) };
                menu_item.setToolTip(Some(&NSString::from_str(tr("クリックで起動", "Click to launch"))));
                // 画像が読めないときは名前だけで出す。メニューの表示を止める理由にはしない
                if let Some(image) = NSImage::initWithContentsOfFile(
                    NSImage::alloc(),
                    &NSString::from_str(&item.entry.icon_path.to_string_lossy()),
                ) {
                    image.setSize(NSSize::new(16., 16.));
                    menu_item.setImage(Some(&image));
                }
            }
        }
        self.separator();
        self.add(tr("今すぐ同期", "Sync Now"), TAG_SYNC, "");
        self.add(tr("DockMorph を開く…", "Open DockMorph…"), TAG_OPEN, ",");
        self.separator();
        match login_item {
            Some(on) => {
                let item = self.add(tr("ログイン時に起動", "Launch at Login"), TAG_LOGIN, "");
                item.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
            }
            None => self.add_label(tr("ログイン時に起動(.app から起動したときのみ)", "Launch at Login (only when run as an .app)")),
        }
        if updates {
            self.add(tr("アップデートを確認…", "Check for Updates…"), TAG_UPDATE, "");
        } else {
            self.add_label(tr("アップデートの確認(配布版のみ)", "Update checking (release builds only)"));
        }
        self.add(tr("DockMorph を終了", "Quit DockMorph"), TAG_QUIT, "q");
    }
}
