import AppKit
import ImageIO

private struct Frames {
    static let maxPixels = 256
    static let minPixels = 64
    /// 展開したコマの合計の上限。コマは Dock の補助プロセスの中に持つので、長い GIF でメモリを食い尽くさないようにする
    static let budgetBytes = 64 * 1024 * 1024
    var images: [NSImage] = []
    var delays: [TimeInterval] = []

    init?(url: URL) {
        guard let src = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        let count = CGImageSourceGetCount(src)
        guard count > 0 else { return nil }
        // タイルの最大表示(拡大時 128pt の 2 倍)を超える分は縮める。コマ数が多くて上限を超えるなら、さらに縮め、
        // それでも足りなければコマを間引く(間引いたコマの表示時間は前のコマに足して、全体の速さを保つ)
        let fit = Int((Double(Frames.budgetBytes) / Double(4 * count)).squareRoot())
        let side = min(Frames.maxPixels, max(Frames.minPixels, fit))
        let stride = max(1, Int((Double(4 * count * side * side) / Double(Frames.budgetBytes)).rounded(.up)))
        let thumb: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: side,
        ]
        for i in 0..<count {
            let props = CGImageSourceCopyPropertiesAtIndex(src, i, nil) as? [CFString: Any]
            let gif = props?[kCGImagePropertyGIFDictionary] as? [CFString: Any]
            let d = (gif?[kCGImagePropertyGIFUnclampedDelayTime] as? Double)
                ?? (gif?[kCGImagePropertyGIFDelayTime] as? Double) ?? 0.1
            // 0 や極端に短い値はブラウザと同じく 0.1 秒に寄せる。Dock の描き直しを詰まらせないため
            let delay = d < 0.02 ? 0.1 : d
            if i % stride != 0 {
                if !delays.isEmpty { delays[delays.count - 1] += delay }
                continue
            }
            let width = (props?[kCGImagePropertyPixelWidth] as? Int) ?? 0
            let height = (props?[kCGImagePropertyPixelHeight] as? Int) ?? 0
            // 大きいコマは元の大きさで展開せず、縮小版を直接作る
            let cg = max(width, height) > side
                ? CGImageSourceCreateThumbnailAtIndex(src, i, thumb as CFDictionary)
                : CGImageSourceCreateImageAtIndex(src, i, nil)
            guard let cg else { continue }
            images.append(NSImage(cgImage: cg, size: NSSize(width: cg.width, height: cg.height)))
            delays.append(delay)
        }
        if images.isEmpty { return nil }
        // 1 コマだけなら描き直す必要がない。差し替えの検知のためにゆっくり回す
        if images.count == 1 { delays = [2] }
    }
}

/// Dock がピン留めのタイルのために読み込むプラグイン。画像は起動用アプリの Info.plist の DMTileImage を読む。
/// DockMorph が画像を差し替えたら、次の周回で読み直す(Dock の再起動は要らない)
@objc(DMAnimTilePlugin)
final class DMAnimTilePlugin: NSObject, NSDockTilePlugIn {
    private var tile: NSDockTile?
    private let view = NSImageView()
    private var frames: Frames?
    private var index = 0
    private var loadedDate: Date?
    private var generation = 0
    /// 画面に出ていない理由。空でなければコマ送りを止める。Dock はコマごとに display() の結果を複製して縮小版を
    /// 作り直すので(実測)、誰も見ていない間も回し続けると Dock の CPU を使い続ける
    private var hiddenReasons: Set<String> = []
    private var observers: [(NotificationCenter, NSObjectProtocol)] = []

    /// 起動用アプリの Info.plist の DMTileImage(DockMorph のデータの場所)。画像を変えても起動用アプリを署名し直さずに済む
    private lazy var imageURL: URL = {
        let launcher = Bundle(for: DMAnimTilePlugin.self).bundleURL.deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        if let path = Bundle(url: launcher)?.infoDictionary?["DMTileImage"] as? String {
            return URL(fileURLWithPath: path)
        }
        // DMTileImage を書く前に作られた起動用アプリ
        return launcher.appendingPathComponent("Contents/Resources/tile-image")
    }()

    func setDockTile(_ dockTile: NSDockTile?) {
        generation += 1
        tile = dockTile
        guard let dockTile else {
            stopObserving()
            return
        }
        view.imageScaling = .scaleProportionallyUpOrDown
        dockTile.contentView = view
        reloadIfChanged()
        if observers.isEmpty { observeVisibility() }
        step(generation)
    }

    private func observeVisibility() {
        let workspace = NSWorkspace.shared.notificationCenter
        let distributed = DistributedNotificationCenter.default()
        let pairs: [(NotificationCenter, Notification.Name, String, Bool)] = [
            (workspace, NSWorkspace.screensDidSleepNotification, "sleep", true),
            (workspace, NSWorkspace.screensDidWakeNotification, "sleep", false),
            // ファストユーザスイッチで別のユーザーに切り替わっている間
            (workspace, NSWorkspace.sessionDidResignActiveNotification, "session", true),
            (workspace, NSWorkspace.sessionDidBecomeActiveNotification, "session", false),
            (distributed, Notification.Name("com.apple.screenIsLocked"), "lock", true),
            (distributed, Notification.Name("com.apple.screenIsUnlocked"), "lock", false),
        ]
        observers = pairs.map { center, name, reason, hidden in
            (center, center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                self?.setHidden(reason, hidden)
            })
        }
    }

    private func stopObserving() {
        for (center, token) in observers { center.removeObserver(token) }
        observers = []
        hiddenReasons = []
    }

    private func setHidden(_ reason: String, _ hidden: Bool) {
        let wasHidden = !hiddenReasons.isEmpty
        if hidden { hiddenReasons.insert(reason) } else { hiddenReasons.remove(reason) }
        guard wasHidden != !hiddenReasons.isEmpty else { return }
        // 予約済みの次のコマを無効にする。再開時は止めたコマの続きから回す
        generation += 1
        if hiddenReasons.isEmpty { step(generation) }
    }

    private func reloadIfChanged() {
        let date = (try? FileManager.default.attributesOfItem(atPath: imageURL.path))?[.modificationDate] as? Date
        guard date != loadedDate else { return }
        loadedDate = date
        frames = Frames(url: imageURL)
        index = 0
        if frames == nil { NSLog("DockMorph: cannot read the tile image: \(imageURL.path)") }
    }

    private func step(_ gen: Int) {
        guard gen == generation, hiddenReasons.isEmpty, let tile else { return }
        if index == 0 { reloadIfChanged() }
        guard let frames else {
            // 起動時に画像がまだ読めなくても、後から配置・修復された画像を拾い直す。
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in self?.step(gen) }
            return
        }
        view.image = frames.images[index]
        tile.display()
        let delay = frames.delays[index]
        index = (index + 1) % frames.images.count
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in self?.step(gen) }
    }
}
