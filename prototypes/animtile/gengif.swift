import AppKit
import ImageIO
import UniformTypeIdentifiers
// 色相を 6 段階に回すだけの検証用 GIF。中央の色でコマを判定する
let out = URL(fileURLWithPath: CommandLine.arguments[1])
let n = 6
let dst = CGImageDestinationCreateWithURL(out as CFURL, UTType.gif.identifier as CFString, n, nil)!
CGImageDestinationSetProperties(dst, [kCGImagePropertyGIFDictionary: [kCGImagePropertyGIFLoopCount: 0]] as CFDictionary)
for i in 0..<n {
    let ctx = CGContext(data: nil, width: 256, height: 256, bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.setFillColor(NSColor(hue: CGFloat(i) / CGFloat(n), saturation: 0.9, brightness: 0.95, alpha: 1).cgColor)
    ctx.fill(CGRect(x: 0, y: 0, width: 256, height: 256))
    CGImageDestinationAddImage(dst, ctx.makeImage()!, [kCGImagePropertyGIFDictionary: [kCGImagePropertyGIFDelayTime: 0.25]] as CFDictionary)
}
print(CGImageDestinationFinalize(dst))
