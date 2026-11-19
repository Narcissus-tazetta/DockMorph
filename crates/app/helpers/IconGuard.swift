import AppKit
import Foundation
import Darwin

struct GuardError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

/// 表示言語。DockMorph の設定(登録ファイルと同じ場所の settings.json)で選ばれた言語、無ければ macOS の言語
let japanese: Bool = {
    let args = CommandLine.arguments
    if args.count > 6 {
        let settings = URL(fileURLWithPath: args[6]).deletingLastPathComponent().appendingPathComponent("settings.json")
        do {
            let object = try JSONSerialization.jsonObject(with: try Data(contentsOf: settings)) as? [String: Any]
            if let language = object?["language"] as? String { return language == "japanese" }
        } catch CocoaError.fileReadNoSuchFile {
        } catch {
            fputs("Cannot read the DockMorph settings, so the macOS language is used: \(error.localizedDescription)\n", stderr)
        }
    }
    return Locale.preferredLanguages.first?.hasPrefix("ja") ?? false
}()

func tr(_ ja: String, _ en: String) -> String { japanese ? ja : en }

struct Target {
    let pid: pid_t
    let seconds: UInt64
    let micros: UInt64
    let bundle: URL

    func isCurrent(startup: Bool = false) -> Bool {
        var info = proc_bsdinfo()
        let size = Int32(MemoryLayout.size(ofValue: info))
        let read = withUnsafeMutablePointer(to: &info) { proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, $0, size) }
        guard read == size && info.pbi_start_tvsec == seconds && info.pbi_start_tvusec == micros else { return false }
        if startup {
            var path = [CChar](repeating: 0, count: Int(MAXPATHLEN) * 4)
            guard proc_pidpath(pid, &path, UInt32(path.count)) > 0 else { return false }
            return URL(fileURLWithPath: String(cString: path)).resolvingSymlinksInPath() == Bundle(url: bundle)?.executableURL?.resolvingSymlinksInPath()
        }
        return NSRunningApplication(processIdentifier: pid)?.bundleURL?.resolvingSymlinksInPath() == bundle.resolvingSymlinksInPath()
    }
}

final class Inspector {
    private let session: URLSession
    private let discovery: URL
    private let port: Int
    private var socket: URLSessionWebSocketTask?
    private var sequence = 0

    init(port: Int = 9229) {
        self.port = port
        discovery = URL(string: "http://127.0.0.1:\(port)/json/list")!
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 0.5
        config.connectionProxyDictionary = [:]
        session = URLSession(configuration: config)
    }

    func endpoints() async throws -> [[String: Any]] {
        let (data, response) = try await session.data(from: discovery)
        guard (response as? HTTPURLResponse)?.statusCode == 200,
              let entries = try JSONSerialization.jsonObject(with: data) as? [[String: Any]] else {
            throw GuardError(message: tr("デバッグ接続先の応答が不正です", "The debugging endpoint returned an invalid response"))
        }
        return entries
    }

    func available() async throws -> Bool {
        do {
            _ = try await endpoints()
            return true
        } catch let error as URLError where error.code == .cannotConnectToHost {
            return false
        }
    }

    func connect(to target: Target, startup: Bool = false) async throws {
        let framework = target.bundle.appendingPathComponent("Contents/Frameworks/Electron Framework.framework/Versions/A/Electron Framework")
        let binary = try Data(contentsOf: framework, options: .mappedIfSafe)
        let sentinel = Data("dL7pKGdnNz796PbbjQWNKmHXBZaB9tsX".utf8)
        guard let marker = binary.range(of: sentinel), binary.count > marker.upperBound + 5,
              binary[marker.upperBound] == 1, binary[marker.upperBound + 1] > 3,
              binary[marker.upperBound + 5] == Character("1").asciiValue else {
            throw GuardError(message: tr("このバージョンの Antigravity は起動中のアイコン制御に対応していません", "This version of Antigravity does not support the running-icon control"))
        }
        if !startup {
            // 既存のデバッガを切断しない。自分で開いた待受だけを閉じる。
            guard try await !available() else { throw GuardError(message: tr("ポート \(port) が使用中です。既存のデバッグ接続は変更していません", "Port \(port) is in use. The existing debugging connection was left unchanged")) }
            guard target.isCurrent() else { throw GuardError(message: tr("対象のアプリは終了または再起動しています", "The app has quit or restarted")) }
            guard kill(target.pid, SIGUSR1) == 0 else { throw GuardError(message: tr("対象のアプリへ接続を要求できません (errno \(errno))", "Cannot ask the app to open a debugging connection (errno \(errno))")) }
        }
        // 起動時は、更新直後の初回起動で macOS がアプリを検査している間も待つ(数十秒かかることがある)。
        // その間にアプリが終了したら待たない
        let deadline = Date().addingTimeInterval(startup ? 60 : 10)
        var entries: [[String: Any]] = []
        while true {
            do {
                entries = try await endpoints()
                break
            } catch let error as URLError where error.code == .cannotConnectToHost || error.code == .timedOut {
                guard target.isCurrent(startup: startup) else { throw GuardError(message: tr("対象のアプリは終了または再起動しています", "The app has quit or restarted")) }
                guard Date() < deadline else {
                    throw GuardError(message: startup
                        ? tr("Antigravity のデバッグ接続が 60 秒たっても開きません", "Antigravity's debugging connection did not open within 60 seconds")
                        : tr("Antigravity のデバッグ接続が開きません。あとから開いた場合に備え、Antigravity を再起動してください", "Antigravity's debugging connection did not open. Restart Antigravity in case it opens later"))
                }
                try await Task.sleep(nanoseconds: 100_000_000)
            }
        }
        // 起動時に作った inspector は再接続時にもタイトルに PID を含まないため、待受ソケットの所有者で照合する。
        let owner = Process()
        owner.executableURL = URL(fileURLWithPath: "/usr/sbin/lsof")
        owner.arguments = ["-nP", "-a", "-p", String(target.pid), "-iTCP:\(port)", "-sTCP:LISTEN", "-Fp"]
        let output = Pipe()
        owner.standardOutput = output
        try owner.run()
        let data = output.fileHandleForReading.readDataToEndOfFile()
        owner.waitUntilExit()
        guard owner.terminationStatus == 0, String(decoding: data, as: UTF8.self).split(separator: "\n").contains(Substring("p\(target.pid)")) else {
            throw GuardError(message: tr("デバッグポートを対象アプリが使用していません", "The debugging port is not used by the app"))
        }
        guard let entry = entries.first,
              let address = entry["webSocketDebuggerUrl"] as? String, let url = URL(string: address),
              url.scheme == "ws", url.host == "127.0.0.1", url.port == port, target.isCurrent(startup: startup) else {
            throw GuardError(message: tr("対象アプリのデバッグ接続が開きません。アプリの更新で接続が無効になった可能性があります", "Cannot open the app's debugging connection. An app update may have disabled it"))
        }
        socket = session.webSocketTask(with: url)
        socket!.resume()
    }

    var connected: Bool { socket != nil }

    func command(_ method: String) async throws {
        _ = try await request(method, params: [:])
    }

    private func request(_ method: String, params: [String: Any]) async throws -> [String: Any] {
        sequence += 1
        let request: [String: Any] = ["id": sequence, "method": method, "params": params]
        let data = try JSONSerialization.data(withJSONObject: request)
        try await socket!.send(.string(String(decoding: data, as: UTF8.self)))
        while true {
            let message = try await socket!.receive()
            let data: Data
            switch message {
            case .string(let text): data = Data(text.utf8)
            case .data(let bytes): data = bytes
            @unknown default: throw GuardError(message: tr("デバッグ接続から未知のメッセージを受信しました", "Received an unknown message from the debugging connection"))
            }
            guard let response = try JSONSerialization.jsonObject(with: data) as? [String: Any] else { throw GuardError(message: tr("デバッグ接続の応答が不正です", "Invalid response from the debugging connection")) }
            if response["method"] as? String == "Debugger.paused" { paused = true }
            if response["id"] as? Int != sequence { continue }
            guard response["error"] == nil, let result = response["result"] as? [String: Any] else {
                throw GuardError(message: tr("デバッグ接続のコマンドに失敗しました: \(method)", "A debugging command failed: \(method)"))
            }
            return result
        }
    }

    func evaluate(_ expression: String) async throws -> Any {
        let result = try await request("Runtime.evaluate", params: ["expression": expression, "returnByValue": true])
        guard result["exceptionDetails"] == nil, let value = (result["result"] as? [String: Any])?["value"] else {
            throw GuardError(message: tr("起動中のアイコン制御に失敗しました。アプリの更新で API が変わった可能性があります", "The running-icon control failed. An app update may have changed the API"))
        }
        return value
    }

    func pauseAtEntry() async throws {
        try await command("Debugger.enable")
        // runIfWaitingForDebugger の応答と paused イベントの順序に依存しないよう、イベントは request 側で記録する。
        try await command("Runtime.runIfWaitingForDebugger")
        while !paused {
            let message = try await socket!.receive()
            if case .string(let text) = message,
               let response = try JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any],
               response["method"] as? String == "Debugger.paused" { paused = true }
        }
    }

    private var paused = false

    func close() async throws {
        do {
            _ = try await evaluate("setTimeout(() => process.getBuiltinModule('node:inspector').close(), 250); true")
        } catch {
            socket!.cancel(with: .goingAway, reason: nil)
            throw GuardError(message: tr("デバッグ接続を閉じられませんでした。Antigravity を再起動してください", "Could not close the debugging connection. Restart Antigravity"))
        }
        socket!.cancel(with: .normalClosure, reason: nil)
        for _ in 0..<20 {
            try await Task.sleep(nanoseconds: 100_000_000)
            if try await !available() { return }
        }
        throw GuardError(message: tr("デバッグ接続を閉じられませんでした。Antigravity を再起動してください", "Could not close the debugging connection. Restart Antigravity"))
    }
}

@main
struct Main {
    static func main() async {
        let startup = CommandLine.arguments.count > 1 && CommandLine.arguments[1] == "startup"
        let args = CommandLine.arguments
        let startupTarget: Target? = startup && args.count == 8 ? Int32(args[2]).flatMap { pid in
            UInt64(args[3]).flatMap { seconds in UInt64(args[4]).map { micros in
                Target(pid: pid, seconds: seconds, micros: micros, bundle: URL(fileURLWithPath: args[5]))
            } }
        } : nil
        // 最後の手段の見張り。起動時は、アプリが --inspect-brk で止まったまま誰も再開させない状態を残さないため、
        // 時間切れならアプリを終了させる。接続を待つ時間(最大 60 秒)より長くしてある
        let watchdog = DispatchWorkItem {
            if let target = startupTarget, target.isCurrent(startup: true) { kill(target.pid, SIGTERM) }
            fputs(tr("アイコン制御がタイムアウトしました。Antigravity を再起動してください", "The icon control timed out. Restart Antigravity") + "\n", stderr)
            if startup {
                Task { await showStartupFailure(tr("アイコン制御がタイムアウトしたため、起動を中止しました。", "Startup was stopped because the icon control timed out.")); exit(1) }
            } else { exit(1) }
        }
        DispatchQueue.global().asyncAfter(deadline: .now() + (startup ? 90 : 20), execute: watchdog)
        var inspector: Inspector?
        do {
            guard (startup ? args.count == 8 : (7...8).contains(args.count)), ["apply", "restore", "startup"].contains(args[1]), let pid = Int32(args[2]),
                  let seconds = UInt64(args[3]), let micros = UInt64(args[4]) else {
                throw GuardError(message: tr("使い方: DMIconGuard apply|restore|startup pid start-seconds start-micros app-path entries-json [port]", "Usage: DMIconGuard apply|restore|startup pid start-seconds start-micros app-path entries-json [port]"))
            }
            let target = Target(pid: pid, seconds: seconds, micros: micros, bundle: URL(fileURLWithPath: args[5]))
            if args[1] == "restore", !target.isCurrent() { return }
            let port = args.count == 8 ? Int(args[7]) : 9229
            guard let port, (1...65535).contains(port) else { throw GuardError(message: tr("デバッグ接続のポートが不正です", "Invalid debugging port")) }
            let connection = Inspector(port: port)
            inspector = connection
            try await connection.connect(to: target, startup: startup)
            if startup { try await connection.pauseAtEntry() }
            let registry = String(decoding: try JSONSerialization.data(withJSONObject: args[6], options: .fragmentsAllowed), as: UTF8.self)
            let expression = guardExpression(pid: pid, registry: registry, restore: args[1] == "restore")
            _ = try await connection.evaluate(expression)
            if startup { try await connection.command("Debugger.resume") }
            try await connection.close()
            watchdog.cancel()
            print("\(args[1]) \(pid): ok")
        } catch {
            // つないだあとの失敗なら、制御をあきらめてアプリはそのまま動かし、接続を閉じる。
            // DockMorph の常駐中は、起動後の制御(apply)でもう一度試して、失敗を画面に出す
            if let connection = inspector, connection.connected {
                var cleanup: [String] = []
                if startup {
                    do { try await connection.command("Debugger.resume") } catch {
                        cleanup.append(error.localizedDescription)
                        // 再開できなければアプリは止まったままになるので、終了させる
                        if let target = startupTarget, target.isCurrent(startup: true) { kill(target.pid, SIGTERM) }
                    }
                }
                do { try await connection.close() } catch { cleanup.append(error.localizedDescription) }
                watchdog.cancel()
                fputs(([error.localizedDescription] + cleanup).joined(separator: "\n") + "\n", stderr)
                if startup && !cleanup.isEmpty { await showStartupFailure(([error.localizedDescription] + cleanup).joined(separator: "\n")) }
                exit(1)
            }
            watchdog.cancel()
            // つなぐ前の失敗。起動時のアプリは --inspect-brk で止まったまま動けないので、終了させるしかない
            if let target = startupTarget, target.isCurrent(startup: true) { kill(target.pid, SIGTERM) }
            fputs("\(error.localizedDescription)\n", stderr)
            if startup { await showStartupFailure(error.localizedDescription) }
            exit(1)
        }
    }

    static func guardExpression(pid: Int32, registry: String, restore: Bool) -> String {
        return """
            (() => {
              if (process.pid !== \(pid)) throw Error('pid mismatch');
              const electron = process.mainModule.require('electron');
              const dock = electron.app.dock;
              const key = Symbol.for('app.dockmorph.icon-guard');
              const restore = state => {
                clearInterval(state.timer);
                state.dock.setIcon = state.original;
                delete globalThis[key];
                const path = process.mainModule.require('node:path');
                state.original.call(state.dock, electron.nativeImage.createFromPath(path.join(process.resourcesPath, 'app.asar', 'icon.png')));
              };
              \(!restore ? """
              if (!globalThis[key]) {
                const original = dock.setIcon;
                const state = { dock, original, calls: 0 };
                original.call(dock, null);
                dock.setIcon = function () {
                  state.calls++;
                  if (state.calls === 1) console.info('DockMorph: blocked original Dock icon override');
                };
                globalThis[key] = state;
              }
              const state = globalThis[key];
              clearInterval(state.timer);
              // CLI でオフにした場合や DockMorph を終了した場合も、登録が外れたら元の動作へ戻す。
              state.timer = setInterval(() => {
                let entries;
                let keepRunningIcon;
                try {
                  const fs = process.mainModule.require('node:fs');
                  const path = process.mainModule.require('node:path');
                  entries = JSON.parse(fs.readFileSync(\(registry), 'utf8'));
                  if (!Array.isArray(entries)) throw Error('invalid registry');
                  let settings;
                  try {
                    settings = JSON.parse(fs.readFileSync(path.join(path.dirname(\(registry)), 'settings.json'), 'utf8'));
                  } catch (error) {
                    if (error.code !== 'ENOENT') throw error;
                    settings = {};
                  }
                  keepRunningIcon = settings.keepRunningIcon ?? true;
                  if (typeof keepRunningIcon !== 'boolean') throw Error('invalid icon control setting');
                } catch (error) {
                  restore(state);
                  console.error('DockMorph: icon control restored because registration or settings could not be read:', error.name);
                  return;
                }
                if (!keepRunningIcon || !entries.some(entry => entry.bundleId === 'com.google.antigravity' && entry.animation?.direct)) restore(state);
              }, 2000);
              state.timer.unref();
              """ : """
              const state = globalThis[key];
              if (state) restore(state);
              """)
              return true;
            })()
            """
    }

    static func showStartupFailure(_ message: String) async {
        await MainActor.run {
            let alert = NSAlert()
            alert.messageText = tr("Antigravity の起動時のアイコン制御に失敗しました", "The icon control at Antigravity's startup failed")
            alert.informativeText = message + tr("\nDockMorph の「Dock で動かす」をオフにすると、通常の方法で起動できます。", "\nTurn off \"Animate in the Dock\" in DockMorph to launch it the usual way.")
            NSApplication.shared.activate(ignoringOtherApps: true)
            alert.runModal()
        }
    }
}
