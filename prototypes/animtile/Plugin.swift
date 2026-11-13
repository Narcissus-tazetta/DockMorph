import AppKit

private func plog(_ s: String) {
    let line = "\(Date().timeIntervalSince1970) pid=\(getpid()) \(ProcessInfo.processInfo.processName) \(s)\n"
    let url = URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("DMAnimProto/plugin.log")
    if let h = try? FileHandle(forWritingTo: url) { h.seekToEndOfFile(); h.write(line.data(using: .utf8)!); try? h.close() }
    else { try? line.data(using: .utf8)!.write(to: url) }
}

@objc(DMAnimTilePlugin)
final class DMAnimTilePlugin: NSObject, NSDockTilePlugIn {
    var animator: TileAnimator?
    func setDockTile(_ dockTile: NSDockTile?) {
        plog("setDockTile \(dockTile == nil ? "nil" : "tile")")
        guard let dockTile else { animator = nil; return }
        let url = Bundle(for: DMAnimTilePlugin.self).url(forResource: "anim", withExtension: "gif")!
        let a = TileAnimator(tile: dockTile, frames: Frames(url: url))
        var count = 0
        a.log = { s in count += 1; if count <= 3 || count % 20 == 0 { plog(s) } }
        animator = a
        a.start()
    }
}
