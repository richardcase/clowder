// Renders a simple placeholder app icon (a teal rounded square with a white "C") to a PNG.
// Usage: swift scripts/gen-icon.swift <out.png> [size]
import AppKit

let args = CommandLine.arguments
guard args.count >= 2 else { fputs("usage: gen-icon.swift <out.png> [size]\n", stderr); exit(2) }
let outPath = args[1]
let size = args.count >= 3 ? (Int(args[2]) ?? 1024) : 1024
let s = CGFloat(size)

let image = NSImage(size: NSSize(width: s, height: s))
image.lockFocus()
let rect = NSRect(x: s * 0.08, y: s * 0.08, width: s * 0.84, height: s * 0.84)
let path = NSBezierPath(roundedRect: rect, xRadius: s * 0.18, yRadius: s * 0.18)
NSColor(calibratedRed: 0.05, green: 0.55, blue: 0.55, alpha: 1).setFill()
path.fill()
let para = NSMutableParagraphStyle(); para.alignment = .center
let attrs: [NSAttributedString.Key: Any] = [
    .font: NSFont.systemFont(ofSize: s * 0.55, weight: .bold),
    .foregroundColor: NSColor.white,
    .paragraphStyle: para,
]
let c = "C" as NSString
let textSize = c.size(withAttributes: attrs)
c.draw(at: NSPoint(x: (s - textSize.width) / 2, y: (s - textSize.height) / 2), withAttributes: attrs)
image.unlockFocus()

guard let tiff = image.tiffRepresentation,
      let rep = NSBitmapImageRep(data: tiff),
      let png = rep.representation(using: .png, properties: [:]) else {
    fputs("gen-icon: failed to render PNG\n", stderr); exit(1)
}
try! png.write(to: URL(fileURLWithPath: outPath))
