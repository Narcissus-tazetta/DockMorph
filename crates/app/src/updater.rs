use anyhow::{anyhow, Result};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_foundation::{NSBundle, NSString};
use std::ffi::CString;

/// Sparkle(SPUStandardUpdaterController)。リンクはせず、同梱されていれば実行時に読み込む
pub struct Updater(Retained<AnyObject>);

impl Updater {
    /// 配布用ビルド(Info.plist に SUFeedURL がある)だけ起動する。cargo run やローカルビルドでは None
    pub fn start() -> Result<Option<Self>> {
        let bundle = NSBundle::mainBundle();
        let feed = bundle.objectForInfoDictionaryKey(&NSString::from_str("SUFeedURL"));
        if feed.is_none() {
            return Ok(None);
        }
        // 更新元が設定されているのに動かせないのはビルドの不備なので、黙って無効にせず呼び出し側へ伝える
        let frameworks = bundle.privateFrameworksPath().ok_or_else(|| anyhow!("Frameworks フォルダがありません"))?;
        let binary = format!("{frameworks}/Sparkle.framework/Sparkle");
        let c_path = CString::new(binary.clone())?;
        let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW) };
        if handle.is_null() {
            let reason = unsafe { std::ffi::CStr::from_ptr(libc::dlerror()) }.to_string_lossy().into_owned();
            return Err(anyhow!("Sparkle を読み込めません({binary}): {reason}"));
        }
        let class = AnyClass::get(c"SPUStandardUpdaterController")
            .ok_or_else(|| anyhow!("Sparkle に SPUStandardUpdaterController がありません"))?;
        let alloc: Allocated<AnyObject> = unsafe { msg_send![class, alloc] };
        let controller: Retained<AnyObject> = unsafe {
            msg_send![
                alloc,
                initWithStartingUpdater: true,
                updaterDelegate: None::<&AnyObject>,
                userDriverDelegate: None::<&AnyObject>
            ]
        };
        Ok(Some(Updater(controller)))
    }

    pub fn check_for_updates(&self) {
        let _: () = unsafe { msg_send![&*self.0, checkForUpdates: None::<&AnyObject>] };
    }
}
