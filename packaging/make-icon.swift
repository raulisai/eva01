// Dibuja el ícono de EVA01 (un cuadro redondeado con una onda de voz) en
// todos los tamaños que pide macOS y los deja en una carpeta .iconset, lista
// para `iconutil -c icns`. Es vectorial: cambia los números de abajo y se
// regenera igual, sin editor de imágenes.
//
// Uso: swift packaging/make-icon.swift <carpeta.iconset>
//      (packaging/make-icon.sh lo hace completo y deja packaging/AppIcon.icns)

import AppKit

// Alturas de las barras, de 0 a 1 respecto al alto disponible: una onda
// que sube y baja alrededor de la barra central.
let bars: [CGFloat] = [0.22, 0.46, 0.78, 0.46, 0.22]
let topColor = NSColor(srgbRed: 0.36, green: 0.42, blue: 0.98, alpha: 1)
let bottomColor = NSColor(srgbRed: 0.42, green: 0.16, blue: 0.72, alpha: 1)

func render(pixels: Int) -> Data {
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels, bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
        bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    defer { NSGraphicsContext.restoreGraphicsState() }

    let s = CGFloat(pixels)
    // La plantilla de macOS: el cuadro ocupa 824 de 1024 y deja margen para la sombra.
    let inset = s * 100 / 1024
    let body = NSRect(x: inset, y: inset, width: s - 2 * inset, height: s - 2 * inset)
    let radius = body.width * 0.2237
    let shape = NSBezierPath(roundedRect: body, xRadius: radius, yRadius: radius)

    NSGraphicsContext.saveGraphicsState()
    let shadow = NSShadow()
    shadow.shadowColor = NSColor.black.withAlphaComponent(0.35)
    shadow.shadowBlurRadius = s * 0.025
    shadow.shadowOffset = NSSize(width: 0, height: -s * 0.012)
    shadow.set()
    NSColor.black.setFill()
    shape.fill()
    NSGraphicsContext.restoreGraphicsState()

    NSGradient(starting: topColor, ending: bottomColor)!.draw(in: shape, angle: -90)

    // Un brillo suave en la mitad de arriba.
    NSGraphicsContext.saveGraphicsState()
    shape.addClip()
    NSGradient(starting: NSColor.white.withAlphaComponent(0.18), ending: NSColor.white.withAlphaComponent(0))!
        .draw(in: NSRect(x: body.minX, y: body.midY, width: body.width, height: body.height / 2), angle: -90)
    NSGraphicsContext.restoreGraphicsState()

    // La onda.
    let waveWidth = body.width * 0.56
    let barWidth = waveWidth / (CGFloat(bars.count) * 2 - 1)
    let maxHeight = body.height * 0.52
    let startX = body.midX - waveWidth / 2
    NSColor.white.setFill()
    for (i, height) in bars.enumerated() {
        let h = maxHeight * height
        let rect = NSRect(x: startX + CGFloat(i) * barWidth * 2, y: body.midY - h / 2, width: barWidth, height: h)
        NSBezierPath(roundedRect: rect, xRadius: barWidth / 2, yRadius: barWidth / 2).fill()
    }

    return rep.representation(using: .png, properties: [:])!
}

guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write(Data("uso: make-icon.swift <carpeta.iconset>\n".utf8))
    exit(2)
}
let out = URL(fileURLWithPath: CommandLine.arguments[1])
try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)

// (nombre, píxeles) — los diez que exige un .iconset.
let files: [(String, Int)] = [
    ("icon_16x16", 16), ("icon_16x16@2x", 32), ("icon_32x32", 32), ("icon_32x32@2x", 64),
    ("icon_128x128", 128), ("icon_128x128@2x", 256), ("icon_256x256", 256), ("icon_256x256@2x", 512),
    ("icon_512x512", 512), ("icon_512x512@2x", 1024),
]
for (name, pixels) in files {
    try render(pixels: pixels).write(to: out.appendingPathComponent("\(name).png"))
}
