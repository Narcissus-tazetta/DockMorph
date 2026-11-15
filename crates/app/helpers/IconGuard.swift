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

    func isCurrent() -> Bool {
        var info = proc_bsdinfo()
        let size = Int32(MemoryLayout.size(ofValue: info))
        let read = withUnsafeMutablePointer(to: &info) { proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, $0, size) }
        return read == size && info.pbi_start_tvsec == seconds && info.pbi_start_tvusec == micros
            && NSRunningApplication(processIdentifier: pid)?.bundleURL?.resolvingSymlinksInPath() == bundle.resolvingSymlinksInPath()
    }
}

final class Inspector {
    private let session: URLSession
    private let discovery = URL(string: "http://127.0.0.1:9229/json/list")!
    private var socket: URLSessionWebSocketTask?
    private var sequence = 0

    init() {
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

    func connect(to target: Target) async throws {
        let framework = target.bundle.appendingPathComponent("Contents/Frameworks/Electron Framework.framework/Versions/A/Electron Framework")
        let binary = try Data(contentsOf: framework, options: .mappedIfSafe)
        let sentinel = Data("dL7pKGdnNz796PbbjQWNKmHXBZaB9tsX".utf8)
        guard let marker = binary.range(of: sentinel), binary.count > marker.upperBound + 5,
              binary[marker.upperBound] == 1, binary[marker.upperBound + 1] > 3,
              binary[marker.upperBound + 5] == Character("1").asciiValue else {
            throw GuardError(message: "このバージョンの Antigravity は起動中のアイコン制御に対応していません")
        }
        // 既存のデバッガを切断しない。自分で開いた待受だけを閉じる。
        guard try await !available() else { throw GuardError(message: "ポート 9229 が使用中です。既存のデバッグ接続は変更していません") }
        guard target.isCurrent() else { throw GuardError(message: "対象のアプリは終了または再起動しています") }
        guard kill(target.pid, SIGUSR1) == 0 else { throw GuardError(message: "対象のアプリへ接続を要求できません (errno \(errno))") }
        var entries: [[String: Any]] = []
        for _ in 0..<25 {
            do {
                entries = try await endpoints()
            } catch let error as URLError where error.code == .cannotConnectToHost {
                try await Task.sleep(nanoseconds: 100_000_000)
                continue
            }
            break
        }
        guard let entry = entries.first(where: { ($0["title"] as? String)?.hasSuffix("[\(target.pid)]") == true }),
              let address = entry["webSocketDebuggerUrl"] as? String, let url = URL(string: address),
              url.scheme == "ws", url.host == "127.0.0.1", url.port == 9229 else {
            throw GuardError(message: "対象アプリのデバッグ接続が開きません。アプリの更新で接続が無効になった可能性があります")
        }
        socket = session.webSocketTask(with: url)
        socket!.resume()
    }

    func evaluate(_ expression: String) async throws -> Any {
        sequence += 1
        let request: [String: Any] = ["id": sequence, "method": "Runtime.evaluate", "params": ["expression": expression, "returnByValue": true]]
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
            if response["id"] as? Int != sequence { continue }
            guard response["error"] == nil, let result = response["result"] as? [String: Any], result["exceptionDetails"] == nil,
                  let value = (result["result"] as? [String: Any])?["value"] else {
                throw GuardError(message: "起動中のアイコン制御に失敗しました。アプリの更新で API が変わった可能性があります")
            }
            return value
        }
    }

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
        DispatchQueue.global().asyncAfter(deadline: .now() + 10) {
            fputs("アイコン制御がタイムアウトしました。デバッグ接続が残る可能性があるため Antigravity を再起動してください\n", stderr)
            exit(1)
        }
        do {
            let args = CommandLine.arguments
            guard args.count == 7, ["apply", "restore"].contains(args[1]), let pid = Int32(args[2]),
                  let seconds = UInt64(args[3]), let micros = UInt64(args[4]) else {
                throw GuardError(message: "使い方: DMIconGuard apply|restore pid start-seconds start-micros app-path entries-json")
            }
            let target = Target(pid: pid, seconds: seconds, micros: micros, bundle: URL(fileURLWithPath: args[5]))
            if args[1] == "restore", !target.isCurrent() { return }
            let inspector = Inspector()
            try await inspector.connect(to: target)
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
              \(args[1] == "apply" ? """
              if (!globalThis[key]) {
                const original = dock.setIcon;
                const state = { dock, original, calls: 0 };
                original.call(dock, null);
                dock.setIcon = function () { state.calls++; };
                globalThis[key] = state;
              }
              const state = globalThis[key];
              clearInterval(state.timer);
              // CLI でオフにした場合や DockMorph を終了した場合も、登録が外れたら元の動作へ戻す。
              state.timer = setInterval(() => {
                let entries;
                try {
                  entries = JSON.parse(process.mainModule.require('node:fs').readFileSync(\(registry), 'utf8'));
                  if (!Array.isArray(entries)) throw Error('invalid registry');
                } catch (error) {
                  restore(state);
                  console.error('DockMorph: icon control restored because registration could not be read:', error.name);
                  return;
                }
                if (!entries.some(entry => entry.bundleId === 'com.google.antigravity' && entry.animation?.direct)) restore(state);
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
                try await inspector.close()
                throw error
            }
            try await inspector.close()
            print("\(args[1]) \(pid): ok")
        } catch {
            fputs("\(error.localizedDescription)\n", stderr)
            exit(1)
        }
    }
}
