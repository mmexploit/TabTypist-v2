import AppKit
import Foundation

// Borderless NSPanel that renders inline ghost text at the caret position.
final class OverlayWindow: NSPanel {
    private let label: NSTextField

    static let shared: OverlayWindow = OverlayWindow()

    private init() {
        label = NSTextField(labelWithString: "")
        label.font = NSFont.systemFont(ofSize: 14, weight: .regular)
        label.textColor = NSColor.labelColor.withAlphaComponent(0.4)
        label.backgroundColor = .clear
        label.isBezeled = false
        label.isEditable = false
        label.cell?.wraps = false
        label.cell?.truncatesLastVisibleLine = false

        super.init(
            contentRect: .zero,
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )

        level = NSWindow.Level(rawValue: Int(CGWindowLevelForKey(.floatingWindow)) + 1)
        isOpaque = false
        backgroundColor = .clear
        collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        ignoresMouseEvents = true
        hasShadow = false

        contentView = label
    }

    func show(text: String, x: CGFloat, y: CGFloat, caretHeight: CGFloat) {
        // Scale font to match the target app's line height.
        // caretHeight ≈ line height; system font line height ≈ fontSize * 1.33
        let fontSize = max(10, caretHeight * 0.75)
        label.font = NSFont.systemFont(ofSize: fontSize, weight: .regular)
        label.stringValue = text

        let attrs: [NSAttributedString.Key: Any] = [.font: label.font as Any]
        let measured = (text as NSString).size(withAttributes: attrs)
        let panelW = measured.width + 4   // 2px NSTextField inset on each side
        let panelH = max(measured.height, caretHeight)

        // Ghost text sits on the same line as the cursor.
        // y = TOP of caret in Cocoa (y-up); y - caretHeight = bottom of caret row.
        let rawX = x
        let rawY = y - caretHeight

        // Clamp to the primary display's visible area as a safety net for apps
        // (e.g. Electron, terminal) that return degenerate AX caret bounds.
        let screen = NSScreen.screens.first ?? NSScreen.main ?? NSScreen()
        let safe = screen.visibleFrame
        let fx = max(safe.minX, min(rawX, safe.maxX - panelW))
        let fy = max(safe.minY, min(rawY, safe.maxY - panelH))

        fputs("overlay: (\(Int(fx)),\(Int(fy))) \(Int(panelW))×\(Int(panelH)) \"\(text.prefix(30))\"\n", stderr)

        setFrame(NSRect(x: fx, y: fy, width: max(panelW, 20), height: max(panelH, 14)), display: true)
        contentView?.frame = NSRect(origin: .zero, size: frame.size)
        alphaValue = 1
        orderFront(nil)
    }

    func hide() {
        NSAnimationContext.runAnimationGroup({ ctx in
            ctx.duration = 0.08
            animator().alphaValue = 0
        }, completionHandler: {
            self.orderOut(nil)
        })
        KeyCapture.shared.clearCompletion()
    }
}
