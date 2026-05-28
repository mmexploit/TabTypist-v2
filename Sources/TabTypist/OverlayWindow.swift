import AppKit
import Foundation

// Borderless NSPanel that renders inline ghost text at the caret position.
final class OverlayWindow: NSPanel {
    // Hard-disable key/main status. With .nonactivatingPanel the panel can still
    // become the *key* window of our (accessory) app, which on some macOS builds
    // affects how the next key event is routed before our CGEventTap sees it.
    // Cotabby's OverlayController does the same override for the same reason.
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }

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

    func show(text: String, x: CGFloat, y: CGFloat, caretHeight: CGFloat, inputFrame: CGRect? = nil) {
        // Scale font to match the target app's line height.
        // caretHeight ≈ line height; system font line height ≈ fontSize * 1.33
        let fontSize = max(10, caretHeight * 0.75)
        let font = NSFont.systemFont(ofSize: fontSize, weight: .regular)

        // Usable region = focused text field's bounds (if AX gave them to us) intersected
        // with the screen's visible frame. Cotabby calls this the "usable text frame": it's
        // what stops ghost text from rendering past the right edge of the host window.
        let screen = NSScreen.screens.first ?? NSScreen.main ?? NSScreen()
        let safe = screen.visibleFrame
        let usable: CGRect = {
            if let f = inputFrame {
                let padded = f.insetBy(dx: 4, dy: 0)
                let inter = padded.intersection(safe)
                return inter.isEmpty ? safe : inter
            }
            return safe
        }()

        // Measure the single-line width. If the completion fits on the current line within
        // the room from caret to right edge, render inline. Otherwise wrap onto subsequent
        // lines (first line indented to caret, overflow flush with the field's left edge).
        let singleLineW = (text as NSString).size(withAttributes: [.font: font]).width + 4
        let availableInline = max(20, usable.maxX - x)

        if singleLineW <= availableInline {
            renderSingleLine(text: text, font: font, caretX: x, caretY: y,
                             caretHeight: caretHeight, usable: usable, panelW: singleLineW)
        } else {
            renderWrapped(text: text, font: font, caretX: x, caretY: y,
                          caretHeight: caretHeight, usable: usable)
        }
    }

    private func renderSingleLine(
        text: String, font: NSFont, caretX: CGFloat, caretY: CGFloat,
        caretHeight: CGFloat, usable: CGRect, panelW: CGFloat
    ) {
        label.font = font
        label.usesSingleLineMode = true
        label.cell?.wraps = false
        label.cell?.truncatesLastVisibleLine = false
        label.cell?.lineBreakMode = .byClipping
        label.maximumNumberOfLines = 1
        label.stringValue = text

        let panelH = max((text as NSString).size(withAttributes: [.font: font]).height, caretHeight)
        let rawY = caretY - caretHeight
        let fx = max(usable.minX, min(caretX, usable.maxX - panelW))
        let fy = max(usable.minY, min(rawY, usable.maxY - panelH))

        fputs("overlay(1L): (\(Int(fx)),\(Int(fy))) \(Int(panelW))×\(Int(panelH)) \"\(text.prefix(30))\"\n", stderr)

        setFrame(NSRect(x: fx, y: fy, width: max(panelW, 20), height: max(panelH, 14)), display: true)
        contentView?.frame = NSRect(origin: .zero, size: frame.size)
        alphaValue = 1
        orderFront(nil)
    }

    private func renderWrapped(
        text: String, font: NSFont, caretX: CGFloat, caretY: CGFloat,
        caretHeight: CGFloat, usable: CGRect
    ) {
        // Multi-line layout. Panel spans the full usable width inside the field. First line
        // is indented to caret X via NSParagraphStyle.firstLineHeadIndent so the ghost text
        // starts cleanly beside the cursor; overflow lines flush to the field's left edge.
        let panelX = usable.minX
        let panelW = max(40, usable.width)
        let firstLineIndent = max(0, caretX - panelX)

        let para = NSMutableParagraphStyle()
        para.firstLineHeadIndent = firstLineIndent
        para.headIndent = 0
        para.lineBreakMode = .byWordWrapping
        para.lineSpacing = 0

        let attrStr = NSAttributedString(string: text, attributes: [
            .font: font,
            .foregroundColor: NSColor.labelColor.withAlphaComponent(0.4),
            .paragraphStyle: para,
        ])

        label.font = font
        label.usesSingleLineMode = false
        label.maximumNumberOfLines = 0
        label.cell?.wraps = true
        label.cell?.truncatesLastVisibleLine = false
        label.cell?.lineBreakMode = .byWordWrapping
        label.attributedStringValue = attrStr

        // Measure wrapped height with the paragraph style applied so firstLineHeadIndent
        // is accounted for. 4pt padding to match NSTextField's internal cell inset.
        let measureWidth = max(20, panelW - 4)
        let bounds = attrStr.boundingRect(
            with: CGSize(width: measureWidth, height: .greatestFiniteMagnitude),
            options: [.usesLineFragmentOrigin, .usesFontLeading]
        )
        let panelH = max(ceil(bounds.height) + 2, caretHeight)

        // Anchor: top of the first wrapped line aligns with the top of the caret (caretY).
        // In Cocoa (y-up) the panel's origin Y is caretY - panelH.
        let rawY = caretY - panelH
        let fx = max(usable.minX, min(panelX, usable.maxX - panelW))
        let fy = max(usable.minY, min(rawY, usable.maxY - panelH))

        fputs("overlay(ML): (\(Int(fx)),\(Int(fy))) \(Int(panelW))×\(Int(panelH)) indent=\(Int(firstLineIndent)) \"\(text.prefix(30))\"\n", stderr)

        setFrame(NSRect(x: fx, y: fy, width: panelW, height: panelH), display: true)
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
