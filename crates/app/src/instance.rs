//! 2 つ目の DockMorph を起動しようとしたときは、動いている方にウィンドウを開かせて自分は終わる

use crate::menubar::Command;
use block2::RcBlock;
use futures::channel::mpsc::UnboundedSender;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2_foundation::{NSDistributedNotificationCenter, NSString};

fn name() -> Retained<NSString> {
    NSString::from_str("app.dockmorph.DockMorph.open-window")
}

pub fn ask_running_to_open() {
    let center = NSDistributedNotificationCenter::defaultCenter();
    unsafe { center.postNotificationName_object_userInfo_deliverImmediately(&name(), None, None, true) };
}

/// 返した token を持っている間だけ受け取る
pub fn listen(tx: UnboundedSender<Command>) -> Retained<ProtocolObject<dyn NSObjectProtocol>> {
    let block = RcBlock::new(move |_| {
        if let Err(e) = tx.unbounded_send(Command::Open) {
            eprintln!("Cannot pass the open-window request to the app: {e}");
        }
    });
    let center = NSDistributedNotificationCenter::defaultCenter();
    unsafe { center.addObserverForName_object_queue_usingBlock(Some(&name()), None, None, &block) }
}
