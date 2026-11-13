import AppKit
import Darwin

public struct DockMorphError: Error, CustomStringConvertible {
    public let description: String
    public init(_ description: String) { self.description = description }
}

public enum Mode: String, Codable, Sendable {
    // 本物のバンドル直下に Icon\r を付ける。複製を持たないので更新とのズレが起きない
    case inPlace
    // 書き込めない本物のための代替。APFS クローンに Icon\r を付けて起動する
    case clone
}

public struct Entry: Codable, Sendable {
    public var mode: Mode
    public var bundleID: String
    public var sourcePath: String
    public var iconPath: String
    public var fingerprint: String
}

public enum SyncResult: Sendable {
    case upToDate(String)
    case recloned(String)
    case reapplied(String)
    case skippedRunning(String)
}

public struct DockMorph {
    public let home: URL

    // DOCKMORPH_HOME は検証時に実環境の設定を汚さないための上書き口
    public init() {
        if let override = ProcessInfo.processInfo.environment["DOCKMORPH_HOME"] {
            home = URL(fileURLWithPath: override, isDirectory: true)
        } else {
            home = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
                .appendingPathComponent("DockMorph", isDirectory: true)
        }
    }

    var clonesDir: URL { home.appendingPathComponent("Apps", isDirectory: true) }
    var iconsDir: URL { home.appendingPathComponent("Icons", isDirectory: true) }
    var entriesFile: URL { home.appendingPathComponent("entries.json") }

    public func clonePath(for entry: Entry) -> URL {
        clonesDir.appendingPathComponent(URL(fileURLWithPath: entry.sourcePath).lastPathComponent, isDirectory: true)
    }

    public func loadEntries() throws -> [Entry] {
        guard FileManager.default.fileExists(atPath: entriesFile.path) else { return [] }
        return try JSONDecoder().decode([Entry].self, from: Data(contentsOf: entriesFile))
    }

    func saveEntries(_ entries: [Entry]) throws {
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        let enc = JSONEncoder()
        enc.outputFormatting = [.prettyPrinted, .sortedKeys]
        try enc.encode(entries).write(to: entriesFile, options: .atomic)
    }

    // 更新検知は版数と実行ファイルの mtime で行う。Sparkle 等は差し替え時に両方か片方を必ず変える
    static func fingerprint(of app: URL) throws -> (bundleID: String, fingerprint: String) {
        guard let bundle = Bundle(url: app), let id = bundle.bundleIdentifier,
              let exec = bundle.executableURL
        else { throw DockMorphError("アプリとして読めません: \(app.path)") }
        let short = bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
        let build = bundle.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? ""
        let mtime = try exec.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate?
            .timeIntervalSince1970 ?? 0
        return (id, "\(short)|\(build)|\(Int(mtime))")
    }

    func isRunning(clone: URL) -> Bool {
        NSWorkspace.shared.runningApplications.contains { $0.bundleURL?.standardizedFileURL.path == clone.standardizedFileURL.path }
    }

    func makeClone(source: URL, destination: URL, icon: URL) throws {
        try FileManager.default.createDirectory(at: destination.deletingLastPathComponent(), withIntermediateDirectories: true)
        if FileManager.default.fileExists(atPath: destination.path) {
            try FileManager.default.removeItem(at: destination)
        }
        // 実コピーにフォールバックしない: 容量を食う上、更新追従の前提(安価な再作成)が崩れるため
        guard clonefile(source.path, destination.path, 0) == 0 else {
            throw DockMorphError("クローン失敗 (errno \(errno): \(String(cString: strerror(errno)))): \(source.path) → \(destination.path)")
        }
        try applyIcon(icon, to: destination)
    }

    func applyIcon(_ icon: URL, to app: URL) throws {
        guard let image = NSImage(contentsOf: icon) else { throw DockMorphError("画像として読めません: \(icon.path)") }
        guard NSWorkspace.shared.setIcon(image, forFile: app.path, options: []) else {
            throw DockMorphError("アイコン設定に失敗 (書き込み権限または App Management の許可が必要な可能性): \(app.path)")
        }
    }

    func clearIcon(of app: URL) throws {
        guard NSWorkspace.shared.setIcon(nil, forFile: app.path, options: []) else {
            throw DockMorphError("アイコンの解除に失敗: \(app.path)")
        }
    }

    // FinderInfo の kHasCustomIcon ビット(8バイト目の 0x04)。アプリ更新でバンドルが差し替わると消える
    static func hasCustomIcon(_ app: URL) -> Bool {
        var info = [UInt8](repeating: 0, count: 32)
        guard getxattr(app.path, "com.apple.FinderInfo", &info, 32, 0, 0) == 32 else { return false }
        return info[8] & 0x04 != 0
    }

    public struct AddResult: Sendable {
        public let entry: Entry
        // in-place を試みて失敗しクローンに切り替えた場合の理由。黙って方式が変わらないよう呼び出し側に必ず見せる
        public let fallbackReason: String?
    }

    public func add(app source: URL, image: URL, forceClone: Bool = false) throws -> AddResult {
        var entries = try loadEntries()
        let (bundleID, fp) = try Self.fingerprint(of: source)
        if entries.contains(where: { $0.bundleID == bundleID }) {
            throw DockMorphError("登録済みです: \(bundleID) (変更するなら remove してから add)")
        }
        try FileManager.default.createDirectory(at: iconsDir, withIntermediateDirectories: true)
        let storedIcon = iconsDir.appendingPathComponent("\(bundleID).\(image.pathExtension)")
        try? FileManager.default.removeItem(at: storedIcon)
        try FileManager.default.copyItem(at: image, to: storedIcon)

        var entry = Entry(mode: .inPlace, bundleID: bundleID, sourcePath: source.path, iconPath: storedIcon.path, fingerprint: fp)
        var fallbackReason: String?
        if !forceClone {
            // 既存のカスタムアイコンを黙って上書きすると remove で元に戻せなくなる
            if Self.hasCustomIcon(source) {
                throw DockMorphError("既にカスタムアイコンが設定されています: \(source.path)")
            }
            do {
                try applyIcon(storedIcon, to: source)
            } catch {
                fallbackReason = "\(error)"
                entry.mode = .clone
            }
        } else {
            entry.mode = .clone
        }
        if entry.mode == .clone {
            try makeClone(source: source, destination: clonePath(for: entry), icon: storedIcon)
        }
        entries.append(entry)
        try saveEntries(entries)
        return AddResult(entry: entry, fallbackReason: fallbackReason)
    }

    public func remove(bundleID: String) throws {
        var entries = try loadEntries()
        guard let i = entries.firstIndex(where: { $0.bundleID == bundleID }) else {
            throw DockMorphError("未登録です: \(bundleID)")
        }
        let entry = entries[i]
        switch entry.mode {
        case .inPlace:
            let source = URL(fileURLWithPath: entry.sourcePath)
            if Self.hasCustomIcon(source) { try clearIcon(of: source) }
        case .clone:
            let clone = clonePath(for: entry)
            if isRunning(clone: clone) { throw DockMorphError("クローンが起動中です。終了してから実行してください: \(clone.path)") }
            try? FileManager.default.removeItem(at: clone)
        }
        try? FileManager.default.removeItem(atPath: entry.iconPath)
        entries.remove(at: i)
        try saveEntries(entries)
    }

    public func sync() throws -> [SyncResult] {
        var entries = try loadEntries()
        var results: [SyncResult] = []
        for i in entries.indices {
            let entry = entries[i]
            let source = URL(fileURLWithPath: entry.sourcePath)
            let (_, fp) = try Self.fingerprint(of: source)
            switch entry.mode {
            case .inPlace:
                if Self.hasCustomIcon(source) {
                    results.append(.upToDate(entry.bundleID))
                } else {
                    try applyIcon(URL(fileURLWithPath: entry.iconPath), to: source)
                    entries[i].fingerprint = fp
                    results.append(.reapplied(entry.bundleID))
                }
            case .clone:
                let clone = clonePath(for: entry)
                let cloneExists = FileManager.default.fileExists(atPath: clone.path)
                if fp == entry.fingerprint && cloneExists {
                    results.append(.upToDate(entry.bundleID))
                } else if cloneExists && isRunning(clone: clone) {
                    results.append(.skippedRunning(entry.bundleID))
                } else {
                    try makeClone(source: source, destination: clone, icon: URL(fileURLWithPath: entry.iconPath))
                    entries[i].fingerprint = fp
                    results.append(.recloned(entry.bundleID))
                }
            }
        }
        try saveEntries(entries)
        return results
    }

    public func launch(bundleID: String) async throws {
        guard let entry = try loadEntries().first(where: { $0.bundleID == bundleID }) else {
            throw DockMorphError("未登録です: \(bundleID)")
        }
        let target = entry.mode == .inPlace ? URL(fileURLWithPath: entry.sourcePath) : clonePath(for: entry)
        _ = try await NSWorkspace.shared.openApplication(at: target, configuration: .init())
    }
}
