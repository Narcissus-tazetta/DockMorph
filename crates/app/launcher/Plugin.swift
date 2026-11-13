import AppKit
import ImageIO

private struct Frames {
    static let maxPixels = 256
    var images: [NSImage] = []
    var delays: [TimeInterval] = []

    init?(url: URL) {
        guard let src = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        // コマは Dock のプロセスの中に展開したまま持つ。大きい GIF をそのまま持つと Dock のメモリを食うので、
        // タイルの最大表示(拡大時 128pt の 2 倍)を超える分は読み込み時に縮める
        let thumb: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: Frames.maxPixels,
        ]
        for i in 0..<CGImageSourceGetCount(src) {
            guard let full = CGImageSourceCreateImageAtIndex(src, i, nil) else { continue }
            let cg = max(full.width, full.height) > Frames.maxPixels
                ? (CGImageSourceCreateThumbnailAtIndex(src, i, thumb as CFDictionary) ?? full) : full
            images.append(NSImage(cgImage: cg, size: NSSize(width: cg.width, height: cg.height)))
            let props = CGImageSourceCopyPropertiesAtIndex(src, i, nil) as? [CFString: Any]
            let gif = props?[kCGImagePropertyGIFDictionary] as? [CFString: Any]
            let d = (gif?[kCGImagePropertyGIFUnclampedDelayTime] as? Double)
                ?? (gif?[kCGImagePropertyGIFDelayTime] as? Double) ?? 0.1
            // 0 や極端に短い値はブラウザと同じく 0.1 秒に寄せる。Dock の描き直しを詰まらせないため
            delays.append(d < 0.02 ? 0.1 : d)
        }
        if images.isEmpty { return nil }
        // 1 コマだけなら描き直す必要がない。差し替えの検知のためにゆっくり回す
        if images.count == 1 { delays = [2] }
    }
}

/// Dock がピン留めのタイルのために読み込むプラグイン。画像は起動用アプリの Resources/tile-image を読む。
/// DockMorph が画像を差し替えたら、次の周回で読み直す(Dock の再起動は要らない)
@objc(DMAnimTilePlugin)
final class DMAnimTilePlugin: NSObject, NSDockTilePlugIn {
    private var tile: NSDockTile?
    private let view = NSImageView()
    private var frames: Frames?
    private var index = 0
    private var loadedDate: Date?
    private var generation = 0

    private var imageURL: URL {
        Bundle(for: DMAnimTilePlugin.self).bundleURL
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Resources/tile-image")
    }

    func setDockTile(_ dockTile: NSDockTile?) {
        generation += 1
        tile = dockTile
        guard let dockTile else { return }
        view.imageScaling = .scaleProportionallyUpOrDown
        dockTile.contentView = view
        reloadIfChanged()
        step(generation)
    }

    private func reloadIfChanged() {
        let date = (try? FileManager.default.attributesOfItem(atPath: imageURL.path))?[.modificationDate] as? Date
        guard date != loadedDate else { return }
        loadedDate = date
        frames = Frames(url: imageURL)
        index = 0
        if frames == nil { NSLog("DockMorph: タイルの画像を読めません: \(imageURL.path)") }
    }

    private func step(_ gen: Int) {
        guard gen == generation, let tile else { return }
        if index == 0 { reloadIfChanged() }
        guard let frames else { return }
        view.image = frames.images[index]
        tile.display()
        let delay = frames.delays[index]
        index = (index + 1) % frames.images.count
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in self?.step(gen) }
    }
}
