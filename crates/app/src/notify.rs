//! macOS の通知センターへの通知。ウィンドウを閉じてメニューバーに常駐している間も、本物のタイルが分かれたことを
//! 知らせ、通知の「まとめ直す」ボタンから開き直せるようにする。
//! 通知が許可されていなくても、同じ内容をウィンドウのバナーとメニューバーのメニューにも出している

use crate::menubar::Command;
use block2::RcBlock;
use dockmorph_core::{l, tr};
use futures::channel::mpsc::UnboundedSender;
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_foundation::{NSArray, NSError, NSSet, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationAction,
    UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

const CATEGORY: &str = "app.dockmorph.split";
const ACTION_REJOIN: &str = "rejoin";
/// 通知の識別子に bundle ID を入れて、ボタンが押されたときにどのアプリかを取り出す
const PREFIX: &str = "split.";

define_class!(
    // 通知センターは delegate を弱参照で持つので、Notifier がこのオブジェクトを持ち続ける。
    // 呼び出されるスレッドは決まっていないので、Command はスレッドをまたげる送信口で渡す
    #[unsafe(super(NSObject))]
    #[ivars = UnboundedSender<Command>]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        // DockMorph が前面にいるときも通知を出す。既定では前面のアプリの通知は出ない
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion.call((UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            let id = response.notification().request().identifier().to_string();
            let command = match id.strip_prefix(PREFIX) {
                Some(bundle_id) if response.actionIdentifier().to_string() == ACTION_REJOIN => Command::Rejoin(bundle_id.to_string()),
                _ => Command::Open,
            };
            if let Err(e) = self.ivars().unbounded_send(command) {
                eprintln!("Cannot pass the notification action to the app: {e}");
            }
            completion.call(());
        }
    }
);

pub struct Notifier {
    _delegate: Retained<Delegate>,
    tx: UnboundedSender<Command>,
}

impl Notifier {
    /// 通知センターはアプリのバンドルからしか使えない(cargo run で直接起動すると例外で落ちる)ので、その場合は None
    pub fn start(tx: UnboundedSender<Command>) -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        if !exe.to_string_lossy().contains(".app/Contents/MacOS/") {
            return None;
        }
        let delegate: Retained<Delegate> = {
            let this = Delegate::alloc().set_ivars(tx.clone());
            unsafe { msg_send![super(this), init] }
        };
        UNUserNotificationCenter::currentNotificationCenter().setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        Some(Notifier { _delegate: delegate, tx })
    }

    pub fn split(&self, bundle_id: &str, name: &str) {
        let center = UNUserNotificationCenter::currentNotificationCenter();
        // ボタンの文言を今の表示言語に合わせるため、毎回登録し直す
        let action = UNNotificationAction::actionWithIdentifier_title_options(
            &NSString::from_str(ACTION_REJOIN),
            &NSString::from_str(tr("まとめ直す", "Rejoin")),
            UNNotificationActionOptions::empty(),
        );
        let category = UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
            &NSString::from_str(CATEGORY),
            &NSArray::from_retained_slice(&[action]),
            &NSArray::new(),
            UNNotificationCategoryOptions::empty(),
        );
        center.setNotificationCategories(&NSSet::from_retained_slice(&[category]));

        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&l!("{name} のタイルが分かれました", "{name} has a separate tile")));
        content.setBody(&NSString::from_str(tr(
            "起動用アプリを通さずに開かれたため、動かないタイルが別に出ています。「まとめ直す」でアプリを終了して開き直すと 1 つに戻ります",
            "It was opened without the launcher app, so a non-animated tile appears separately. \"Rejoin\" quits and reopens the app to return to a single tile",
        )));
        content.setCategoryIdentifier(&NSString::from_str(CATEGORY));
        self.post(UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(&format!("{PREFIX}{bundle_id}")), &content, None));
    }

    /// ボタンのない知らせ。ウィンドウを閉じている間に起きた失敗を伝える
    pub fn message(&self, title: &str, body: &str) {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        let id = format!("message.{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        self.post(UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(&id), &content, None));
    }

    /// 初めて呼んだときに通知の許可を求める。許可されていなければ Command::NotifyFailed で理由を返す
    fn post(&self, request: Retained<UNNotificationRequest>) {
        let tx = self.tx.clone();
        let fail = move |reason: String| {
            if let Err(e) = tx.unbounded_send(Command::NotifyFailed(reason)) {
                eprintln!("Cannot pass the notification failure to the app: {e}");
            }
        };
        let authorized = RcBlock::new(move |granted: Bool, error: *mut NSError| {
            if let Some(error) = unsafe { error.as_ref() } {
                return fail(error.localizedDescription().to_string());
            }
            if !granted.as_bool() {
                return fail(tr("通知が許可されていません。システム設定 › 通知 で DockMorph を許可してください", "Notifications are not allowed. Allow DockMorph in System Settings › Notifications").into());
            }
            let fail = fail.clone();
            let added = RcBlock::new(move |error: *mut NSError| {
                if let Some(error) = unsafe { error.as_ref() } {
                    fail(error.localizedDescription().to_string());
                }
            });
            UNUserNotificationCenter::currentNotificationCenter().addNotificationRequest_withCompletionHandler(&request, Some(&added));
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound, &authorized);
    }

    /// まとめ直したあとや、アプリが終了したあとに、古い通知を通知センターに残さない
    pub fn withdraw(&self, bundle_id: &str) {
        let ids = NSArray::from_retained_slice(&[NSString::from_str(&format!("{PREFIX}{bundle_id}"))]);
        UNUserNotificationCenter::currentNotificationCenter().removeDeliveredNotificationsWithIdentifiers(&ids);
    }
}
