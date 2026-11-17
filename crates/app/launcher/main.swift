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

/// DockMorph が Dock のタイルのメニューで「Dock に残す」を押す間、起動中のタイルを出したままにする
let hold = CommandLine.arguments.contains("--dm-hold")

func startupFailure(_ message: String) -> Never {
    let alert = NSAlert()
    alert.messageText = "Antigravity を起動できません"
    alert.informativeText = message
    NSApplication.shared.activate(ignoringOtherApps: true)
    alert.runModal()
    exit(1)
}

func execTarget() {
    guard direct, !hold, !CommandLine.arguments.contains("--dm-register"),
          let exe = Bundle(path: targetPath)?.executableURL?.resolvingSymlinksInPath().path,
          !isRunning(executable: exe) else { return }
    // exec 後のプロセスは外から見ると本物と区別が付かない。DockMorph が「起動用アプリから開かれた(タイルが 1 つ)」と
    // 判定できるよう、環境変数に印を残す。環境変数は、アプリが自分を起動し直したとき(更新後の再起動など)の
    // 新しいプロセスにも引き継がれる。そちらは本物のタイルに出るので、exec しても変わらない pid を値にして見分ける
    setenv("DOCKMORPH_LAUNCHER_PID", String(getpid()), 1)
    var startupArguments: [String] = []
    var iconGuard: Process?
    if targetBundleID == "com.google.antigravity" {
        let helper = Process()
        helper.executableURL = Bundle.main.bundleURL.appendingPathComponent("Contents/Resources/DMIconGuard")
        var processInfo = proc_bsdinfo()
        let size = Int32(MemoryLayout.size(ofValue: processInfo))
        let read = withUnsafeMutablePointer(to: &processInfo) { proc_pidinfo(getpid(), PROC_PIDTBSDINFO, 0, $0, size) }
        guard read == size, let registry = info["DMRegistry"] as? String else {
            startupFailure("起動用アプリの設定を読めません。DockMorph を起動して更新してください。")
        }
        // 通常の起動後ガード(9229)と競合させず、同時起動する別 PID ともポートを分ける。
        let port = 20000 + Int(getpid()) % 40000
        helper.arguments = ["startup", String(getpid()), String(processInfo.pbi_start_tvsec), String(processInfo.pbi_start_tvusec), targetPath, registry, String(port)]
        do { try helper.run() } catch { startupFailure("アイコン制御を開始できません: \(error.localizedDescription)") }
        setenv("DOCKMORPH_ICON_GUARD_PID", String(getpid()), 1)
        iconGuard = helper
        startupArguments = ["--inspect-brk=127.0.0.1:\(port)"]
    }
    var args: [UnsafeMutablePointer<CChar>?] = [strdup(exe)]
    args += (startupArguments + Array(CommandLine.arguments.dropFirst())).map { strdup($0) }
    args.append(nil)
    execv(exe, args)
    let execError = errno
    iconGuard?.terminate()
    unsetenv("DOCKMORPH_ICON_GUARD_PID")
    unsetenv("DOCKMORPH_LAUNCHER_PID")
    // 戻ってきたら失敗。本物を開く通常の経路に回す
    NSLog("DockMorph: exec に失敗 (\(String(cString: strerror(execError)))): \(exe)")
}

execTarget()

final class Delegate: NSObject, NSApplicationDelegate {
    // DockMorph が URL スキーム・書類の既定をこの起動用アプリにしているので、本物へそのまま渡す。
    // 起動時に渡されたものは applicationDidFinishLaunching より前に届く
    var pending: [URL] = []

    func application(_ application: NSApplication, open urls: [URL]) {
        pending += urls
    }

    func applicationDidFinishLaunching(_ n: Notification) {
        // Dock は一度も起動されていないアプリのプラグインを読み込まない(実測)。DockMorph が登録のためだけに起動する
        if CommandLine.arguments.contains("--dm-register") {
            NSApp.terminate(nil)
            return
        }
        if hold {
            // DockMorph が終了させる。DockMorph が途中で落ちても Dock に居残らないよう、自分でも切り上げる
            DispatchQueue.main.asyncAfter(deadline: .now() + 30) { NSApp.terminate(nil) }
            return
        }
        var url = URL(fileURLWithPath: targetPath)
        // 本物が移動されていても、DockMorph の次の同期を待たずに開けるようにする
        if !FileManager.default.fileExists(atPath: targetPath),
           let found = NSWorkspace.shared.urlForApplication(withBundleIdentifier: targetBundleID) {
            url = found
        }
        let done: (NSRunningApplication?, Error?) -> Void = { _, error in
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
        if pending.isEmpty {
            NSWorkspace.shared.openApplication(at: url, configuration: .init(), completionHandler: done)
        } else {
            NSWorkspace.shared.open(pending, withApplicationAt: url, configuration: .init(), completionHandler: done)
        }
    }
}

let app = NSApplication.shared
let delegate = Delegate()
app.delegate = delegate
app.run()
