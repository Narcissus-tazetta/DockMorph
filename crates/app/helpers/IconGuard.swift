import AppKit
import Foundation
import Darwin

struct GuardError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

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
            throw GuardError(message: "デバッグ接続先の応答が不正です")
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
            throw GuardError(message: "このバージョンの Antigravity は起動中のアイコン制御に対応していません")
        }
        if !startup {
            // 既存のデバッガを切断しない。自分で開いた待受だけを閉じる。
            guard try await !available() else { throw GuardError(message: "ポート \(port) が使用中です。既存のデバッグ接続は変更していません") }
            guard target.isCurrent() else { throw GuardError(message: "対象のアプリは終了または再起動しています") }
            guard kill(target.pid, SIGUSR1) == 0 else { throw GuardError(message: "対象のアプリへ接続を要求できません (errno \(errno))") }
        }
        var entries: [[String: Any]] = []
        for _ in 0..<(startup ? 60 : 25) {
            do {
                entries = try await endpoints()
            } catch let error as URLError where error.code == .cannotConnectToHost {
                try await Task.sleep(nanoseconds: 100_000_000)
                continue
            }
            break
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
            throw GuardError(message: "デバッグポートを対象アプリが使用していません")
        }
        guard let entry = entries.first,
              let address = entry["webSocketDebuggerUrl"] as? String, let url = URL(string: address),
              url.scheme == "ws", url.host == "127.0.0.1", url.port == port, target.isCurrent(startup: startup) else {
            throw GuardError(message: "対象アプリのデバッグ接続が開きません。アプリの更新で接続が無効になった可能性があります")
        }
        socket = session.webSocketTask(with: url)
        socket!.resume()
    }

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
            @unknown default: throw GuardError(message: "デバッグ接続から未知のメッセージを受信しました")
            }
            guard let response = try JSONSerialization.jsonObject(with: data) as? [String: Any] else { throw GuardError(message: "デバッグ接続の応答が不正です") }
            if response["method"] as? String == "Debugger.paused" { paused = true }
            if response["id"] as? Int != sequence { continue }
            guard response["error"] == nil, let result = response["result"] as? [String: Any] else {
                throw GuardError(message: "デバッグ接続のコマンドに失敗しました: \(method)")
            }
            return result
        }
    }

    func evaluate(_ expression: String) async throws -> Any {
        let result = try await request("Runtime.evaluate", params: ["expression": expression, "returnByValue": true])
        guard result["exceptionDetails"] == nil, let value = (result["result"] as? [String: Any])?["value"] else {
            throw GuardError(message: "起動中のアイコン制御に失敗しました。アプリの更新で API が変わった可能性があります")
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
            throw GuardError(message: "デバッグ接続を閉じられませんでした。Antigravity を再起動してください")
        }
        socket!.cancel(with: .normalClosure, reason: nil)
        for _ in 0..<20 {
            try await Task.sleep(nanoseconds: 100_000_000)
            if try await !available() { return }
        }
        throw GuardError(message: "デバッグ接続を閉じられませんでした。Antigravity を再起動してください")
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
        let watchdog = DispatchWorkItem {
            if let target = startupTarget, target.isCurrent(startup: true) { kill(target.pid, SIGTERM) }
            fputs("アイコン制御がタイムアウトしました。Antigravity を再起動してください\n", stderr)
            if startup {
                Task { await showStartupFailure("アイコン制御がタイムアウトしたため、起動を中止しました。"); exit(1) }
            } else { exit(1) }
        }
        DispatchQueue.global().asyncAfter(deadline: .now() + 10, execute: watchdog)
        do {
            guard (startup ? args.count == 8 : (7...8).contains(args.count)), ["apply", "restore", "startup"].contains(args[1]), let pid = Int32(args[2]),
                  let seconds = UInt64(args[3]), let micros = UInt64(args[4]) else {
                throw GuardError(message: "使い方: DMIconGuard apply|restore pid start-seconds start-micros app-path entries-json")
            }
            let target = Target(pid: pid, seconds: seconds, micros: micros, bundle: URL(fileURLWithPath: args[5]))
            if args[1] == "restore", !target.isCurrent() { return }
            let port = args.count == 8 ? Int(args[7]) : 9229
            guard let port, (1...65535).contains(port) else { throw GuardError(message: "デバッグ接続のポートが不正です") }
            let inspector = Inspector(port: port)
            try await inspector.connect(to: target, startup: startup)
            if startup { try await inspector.pauseAtEntry() }
            let registry = String(decoding: try JSONSerialization.data(withJSONObject: args[6], options: .fragmentsAllowed), as: UTF8.self)
            let expression = """
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
              \(args[1] != "restore" ? """
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
            do {
                _ = try await inspector.evaluate(expression)
            } catch {
                if startup { try await inspector.command("Debugger.resume") }
                try await inspector.close()
                throw error
            }
            if startup { try await inspector.command("Debugger.resume") }
            try await inspector.close()
            watchdog.cancel()
            print("\(args[1]) \(pid): ok")
        } catch {
            watchdog.cancel()
            if let target = startupTarget, target.isCurrent(startup: true) { kill(target.pid, SIGTERM) }
            fputs("\(error.localizedDescription)\n", stderr)
            if startup { await showStartupFailure(error.localizedDescription) }
            exit(1)
        }
    }

    static func showStartupFailure(_ message: String) async {
        await MainActor.run {
            let alert = NSAlert()
            alert.messageText = "Antigravity の起動時のアイコン制御に失敗しました"
            alert.informativeText = message + "\nDockMorph の「Dock で動かす」をオフにすると、通常の方法で起動できます。"
            NSApplication.shared.activate(ignoringOtherApps: true)
            alert.runModal()
        }
    }
}
