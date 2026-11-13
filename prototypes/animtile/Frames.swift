import AppKit
import ImageIO

struct Frames {
    let images: [NSImage]
    let delays: [TimeInterval]

    init(url: URL) {
        let src = CGImageSourceCreateWithURL(url as CFURL, nil)!
        var images: [NSImage] = []
        var delays: [TimeInterval] = []
        for i in 0..<CGImageSourceGetCount(src) {
            let cg = CGImageSourceCreateImageAtIndex(src, i, nil)!
            images.append(NSImage(cgImage: cg, size: NSSize(width: cg.width, height: cg.height)))
            let props = CGImageSourceCopyPropertiesAtIndex(src, i, nil) as? [CFString: Any]
            let gif = props?[kCGImagePropertyGIFDictionary] as? [CFString: Any]
            let d = (gif?[kCGImagePropertyGIFUnclampedDelayTime] as? Double) ?? (gif?[kCGImagePropertyGIFDelayTime] as? Double) ?? 0.1
            delays.append(max(d, 0.05))
        }
        self.images = images
        self.delays = delays
    }
}

/// NSDockTile にコマを順に描く。アプリ本体とプラグインの両方で使う
final class TileAnimator {
    let tile: NSDockTile
    let frames: Frames
    let view = NSImageView()
    var index = 0
    var log: ((String) -> Void)?

    init(tile: NSDockTile, frames: Frames) {
        self.tile = tile
        self.frames = frames
        view.imageScaling = .scaleProportionallyUpOrDown
        tile.contentView = view
    }

    func start() { step() }

    private func step() {
        view.image = frames.images[index]
        tile.display()
        log?("frame \(index)")
        let delay = frames.delays[index]
        index = (index + 1) % frames.images.count
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in self?.step() }
    }
}
