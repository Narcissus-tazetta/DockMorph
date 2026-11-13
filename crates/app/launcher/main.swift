import AppKit
import Darwin

// Dock にピン留めされる起動用アプリ。タイルは同梱のプラグインが Dock 側で動かす。
// DMDirect のときは同じプロセスのまま本物の実行ファイルに exec する。Dock からは「このタイルのアプリが起動した」と
// 見えるのでタイルが 1 つで済み、プロセスの署名は本物のままなのでキーチェーンも通る(実測)。
// それ以外(データが保護されたアプリ、本物がすでに起動中、本物が見つからない)は本物を開いて即終了する。

let info = Bundle.main.infoDictionary ?? [:]
let targetPath = info["DMTargetApp"] as? String ?? ""
let targetBundleID = info["DMTargetBundleID"] as? String ?? ""
let direct = info["DMDirect"] as? Bool ?? false

/// LaunchServices に問い合わせると、exec の前にこのプロセスが起動用アプリとして登録されかねないので、プロセス一覧を直接見る
func isRunning(executable: String) -> Bool {
    let count = proc_listallpids(nil, 0)
    guard count > 0 else { return false }
    var pids = [pid_t](repeating: 0, count: Int(count) * 2)
    let n = proc_listallpids(&pids, Int32(pids.count * MemoryLayout<pid_t>.size))
    var buf = [CChar](repeating: 0, count: Int(MAXPATHLEN) * 4)
    for pid in pids.prefix(Int(max(n, 0))) where pid > 0 && pid != getpid() {
        if proc_pidpath(pid, &buf, UInt32(buf.count)) > 0, String(cString: buf) == executable {
            return true
        }
    }
    return false
}

func execTarget() {
    guard direct, !CommandLine.arguments.contains("--dm-register"),
          let exe = Bundle(path: targetPath)?.executableURL?.resolvingSymlinksInPath().path,
          !isRunning(executable: exe) else { return }
    // exec 後のプロセスは外から見ると本物と区別が付かない。DockMorph が「起動用アプリから開かれた(タイルが 1 つ)」と
    // 判定できるよう、環境変数に印を残す
    setenv("DOCKMORPH_LAUNCHER", Bundle.main.bundlePath, 1)
    var args: [UnsafeMutablePointer<CChar>?] = [strdup(exe)]
    args += CommandLine.arguments.dropFirst().map { strdup($0) }
    args.append(nil)
    execv(exe, args)
    unsetenv("DOCKMORPH_LAUNCHER")
    // 戻ってきたら失敗。本物を開く通常の経路に回す
    NSLog("DockMorph: exec に失敗 (\(String(cString: strerror(errno)))): \(exe)")
}

execTarget()

final class Delegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ n: Notification) {
        // Dock は一度も起動されていないアプリのプラグインを読み込まない(実測)。DockMorph が登録のためだけに起動する
        if CommandLine.arguments.contains("--dm-register") {
            NSApp.terminate(nil)
            return
        }
        var url = URL(fileURLWithPath: targetPath)
        // 本物が移動されていても、DockMorph の次の同期を待たずに開けるようにする
        if !FileManager.default.fileExists(atPath: targetPath),
           let found = NSWorkspace.shared.urlForApplication(withBundleIdentifier: targetBundleID) {
            url = found
        }
        NSWorkspace.shared.openApplication(at: url, configuration: .init()) { _, error in
            DispatchQueue.main.async {
                if let error {
                    let alert = NSAlert()
                    alert.messageText = "アプリを開けません"
                    alert.informativeText = "\(url.path)\n\(error.localizedDescription)"
                    NSApp.activate(ignoringOtherApps: true)
                    alert.runModal()
                }
                NSApp.terminate(nil)
            }
        }
    }
}

let app = NSApplication.shared
let delegate = Delegate()
app.delegate = delegate
app.run()
