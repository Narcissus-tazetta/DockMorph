import AppKit

// 起動用アプリ自身は常駐しない。本物を前面に出したらすぐ終了し、ピン留めのタイルはプラグインが動かし続ける
final class Delegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ n: Notification) {
        let target = Bundle.main.object(forInfoDictionaryKey: "DMTargetApp") as! String
        let config = NSWorkspace.OpenConfiguration()
        // 起動用アプリと本物が同じ bundle ID のとき、指定しないと「起動済み」とみなされて自分が前に出るだけになる
        config.createsNewApplicationInstance = true
        NSWorkspace.shared.openApplication(at: URL(fileURLWithPath: target), configuration: config) { _, error in
            if let error { NSLog("本物を起動できません: \(error)") }
            DispatchQueue.main.async { NSApp.terminate(nil) }
        }
    }
}

let app = NSApplication.shared
let delegate = Delegate()
app.delegate = delegate
app.run()
