import AppKit

// 使い方: swift scripts/icon.swift <記号の SVG> <出力.iconset>
// メニューバーと同じ記号を、macOS のアプリアイコンの形(角丸の四角)に白抜きで置く
let args = CommandLine.arguments
guard args.count == 3, let glyph = NSImage(contentsOfFile: args[1]) else {
    FileHandle.standardError.write("usage: icon.swift <glyph.svg> <out.iconset>\n".data(using: .utf8)!)
    exit(1)
}
try FileManager.default.createDirectory(atPath: args[2], withIntermediateDirectories: true)

func render(_ px: Int) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8, samplesPerPixel: 4,
                               hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let s = CGFloat(px)
    // Apple のアイコンの雛形と同じ比率: 1024 のうち 824 が本体、角の半径は 185
    let body = NSRect(x: s * 100 / 1024, y: s * 100 / 1024, width: s * 824 / 1024, height: s * 824 / 1024)
    let path = NSBezierPath(roundedRect: body, xRadius: s * 185 / 1024, yRadius: s * 185 / 1024)
    NSGradient(starting: NSColor(srgbRed: 0.42, green: 0.56, blue: 1.0, alpha: 1), ending: NSColor(srgbRed: 0.24, green: 0.30, blue: 0.86, alpha: 1))!
        .draw(in: path, angle: -90)
    let g = body.insetBy(dx: body.width * 0.15, dy: body.height * 0.15)
    // 記号の形だけを使って白で塗る
    let white = NSImage(size: g.size, flipped: false) { r in
        glyph.draw(in: r)
        NSColor.white.set()
        r.fill(using: .sourceIn)
        return true
    }
    white.draw(in: g)
    NSGraphicsContext.current = nil
    return rep.representation(using: .png, properties: [:])!
}

for (name, px) in [("16x16", 16), ("16x16@2x", 32), ("32x32", 32), ("32x32@2x", 64), ("128x128", 128), ("128x128@2x", 256),
                   ("256x256", 256), ("256x256@2x", 512), ("512x512", 512), ("512x512@2x", 1024)] {
    try render(px).write(to: URL(fileURLWithPath: args[2]).appendingPathComponent("icon_\(name).png"))
}
