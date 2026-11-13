import DockMorphCore
import Foundation

let usage = """
usage:
  dockmorph add [--clone] <App.app> <image>
                                    本物にアイコンを付ける。書き込めなければクローンに切り替える(--clone でクローン固定)
  dockmorph list                    登録とクローンの場所を表示
  dockmorph sync                    元アプリの更新を検知して作り直す
  dockmorph launch <bundle-id>      クローンを起動
  dockmorph remove <bundle-id>      クローンと登録を削除
"""

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(1)
}

let args = Array(CommandLine.arguments.dropFirst())
let dm = DockMorph()

do {
    switch (args.first, args.count) {
    case ("add", 3...4):
        let rest = Array(args.dropFirst())
        let forceClone = rest.contains("--clone")
        let paths = rest.filter { $0 != "--clone" }
        guard paths.count == 2 else { fail(usage) }
        let r = try dm.add(app: URL(fileURLWithPath: paths[0]), image: URL(fileURLWithPath: paths[1]), forceClone: forceClone)
        if let reason = r.fallbackReason {
            FileHandle.standardError.write(Data("warning: 本物に書き込めなかったためクローン方式に切り替えました: \(reason)\n".utf8))
        }
        print("added \(r.entry.bundleID) [\(r.entry.mode.rawValue)]")
        if r.entry.mode == .clone { print("clone: \(dm.clonePath(for: r.entry).path)") }
    case ("list", 1):
        for e in try dm.loadEntries() {
            let target = e.mode == .inPlace ? e.sourcePath : dm.clonePath(for: e).path
            print("\(e.bundleID)\t\(e.mode.rawValue)\t\(target)")
        }
    case ("sync", 1):
        for r in try dm.sync() {
            switch r {
            case .upToDate(let id): print("up-to-date \(id)")
            case .recloned(let id): print("recloned   \(id)")
            case .reapplied(let id): print("reapplied  \(id)")
            case .skippedRunning(let id): print("skipped (clone running) \(id)")
            }
        }
    case ("launch", 2):
        try await dm.launch(bundleID: args[1])
    case ("remove", 2):
        try dm.remove(bundleID: args[1])
    default:
        fail(usage)
    }
} catch {
    fail("error: \(error)")
}
