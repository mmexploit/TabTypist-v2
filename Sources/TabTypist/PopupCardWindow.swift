import AppKit

// Floating card shown when the AX caret is unreliable (Firefox, some Electron apps).
// Positioned just below the focused text field instead of inline at the caret.
final class PopupCardWindow: NSPanel {
    override var canBecomeKey: Bool  { false }
    override var canBecomeMain: Bool { false }

    static let shared = PopupCardWindow()

    private let label: NSTextField
    private let card = NSView()

    private init() {
        label = NSTextField(labelWithString: "")
        label.font = NSFont.systemFont(ofSize: 13, weight: .regular)
        label.textColor = NSColor.labelColor.withAlphaComponent(0.5)
        label.backgroundColor = .clear
        label.isBezeled = false
        label.isEditable = false
        label.cell?.wraps = true
        label.cell?.truncatesLastVisibleLine = true
        label.maximumNumberOfLines = 3

        super.init(
            contentRect: NSRect(x: 0, y: 0, width: 300, height: 60),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )

        level = NSWindow.Level(rawValue: Int(CGWindowLevelForKey(.floatingWindow)) + 1)
        isOpaque = false
        // Ghost-text styling: the window itself is clear; a rounded, low-contrast
        // card view holds the label so the fallback reads as floating ghost text,
        // not a hard-edged box stamped over the host app's UI.
        backgroundColor = .clear
        collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        ignoresMouseEvents = true
        hasShadow = false
        card.wantsLayer = true
        card.layer?.cornerRadius = 8
        card.layer?.masksToBounds = true
        card.addSubview(label)
        contentView = card
    }

    /// Known-unreliable bundle IDs that trigger popup mode automatically. Browsers
    /// only: their caret geometry is genuinely absent or fabricated. Electron chat
    /// apps (Slack, Discord, …) are deliberately NOT listed — Chromium's composer
    /// caret rect is usually good, so they render inline at the caret like native
    /// apps, and the per-show geometry gate in shouldUsePopup catches the polls where
    /// the rect is junk (outside the field / implausibly tall) and falls back to the
    /// card just for those.
    static let unreliableBundles: Set<String> = [
        "org.mozilla.firefox",
        "com.google.Chrome",
        "com.microsoft.edgemac",
        "com.brave.Browser",
        "com.operasoftware.Opera",
    ]

    /// True when popup mode should be used for the given app (user-pinned, automatic,
    /// or because the caret geometry fails validation). The geometry gate is cotabby's
    /// caret-quality idea: a caret taller than any plausible text line, or lying
    /// outside the field it supposedly belongs to, is junk — trusting it paints ghost
    /// text over existing content even in apps not on the bundle list.
    static func shouldUsePopup(
        bundleId: String,
        caretHeight: CGFloat,
        caretPoint: NSPoint? = nil,
        inputFrame: CGRect? = nil
    ) -> Bool {
        let pinned = UserDefaults.standard.string(forKey: "overlayMode.\(bundleId)")
        if pinned == "popup" { return true }
        if pinned == "inline" { return false }
        if caretHeight == 0 {
            fputs("PopupCard: popup mode (no caret bounds) bundle=\(bundleId)\n", stderr)
            return true
        }
        if caretHeight > 60 {
            fputs("PopupCard: popup mode (caret height \(caretHeight)) bundle=\(bundleId)\n", stderr)
            return true
        }
        if let p = caretPoint, let f = inputFrame, f.height > 0,
           !f.insetBy(dx: -8, dy: -8).contains(p) {
            fputs("PopupCard: popup mode (caret \(p) outside field \(f)) bundle=\(bundleId)\n", stderr)
            return true
        }
        return unreliableBundles.contains(bundleId)
    }

    func show(text: String, inputFrame: CGRect) {
        guard inputFrame.width > 10 else { return }

        let cardPadding: CGFloat = 8
        let cardGap: CGFloat = 4
        let maxW = min(inputFrame.width - cardPadding * 2, 360)

        label.textColor = OverlayWindow.ghostTextColor()
        label.font = NSFont.systemFont(ofSize: 13, weight: .regular)

        // Include hint pill text if applicable.
        let displayText = OverlayWindow.shouldShowHint() ? text + "  Tab ⇥ " : text
        label.stringValue = displayText

        let measured = (displayText as NSString).boundingRect(
            with: CGSize(width: maxW - cardPadding * 2, height: .greatestFiniteMagnitude),
            options: [.usesLineFragmentOrigin],
            attributes: [.font: label.font!]
        )
        let cardW = max(120, min(measured.width + cardPadding * 2 + 4, maxW))
        let cardH = max(30, measured.height + cardPadding * 2)

        // Prefer just below the field; when there's no room (chat composers like
        // Slack's sit at the very bottom of the screen) flip ABOVE the field instead
        // of clamping into the field, which would cover the text being typed.
        let screen = NSScreen.screens.first(where: { $0.frame.intersects(inputFrame) })
            ?? NSScreen.screens.first
        let visibleMinY = screen?.visibleFrame.minY ?? 0
        let fx = inputFrame.minX + cardPadding
        var fy = inputFrame.minY - cardH - cardGap
        if fy < visibleMinY {
            fy = inputFrame.maxY + cardGap
        }

        setFrame(NSRect(x: fx, y: fy, width: cardW, height: cardH), display: true)
        card.frame = NSRect(origin: .zero, size: frame.size)
        // Resolve the backdrop color each show so light/dark appearance changes stick
        // (layer colors don't track appearance automatically).
        card.layer?.backgroundColor =
            NSColor.windowBackgroundColor.withAlphaComponent(0.85).cgColor
        label.frame = card.bounds.insetBy(dx: cardPadding, dy: cardPadding)
        alphaValue = 1
        orderFront(nil)
    }

    func hide() {
        orderOut(nil)
    }
}

// ── Overlay routing ───────────────────────────────────────────────────────────

/// Single chokepoint that decides inline ghost text vs popup card and presents the
/// completion. Every display path (Rust showOverlay, word-by-word reposition, Apple
/// Intelligence bypass) must come through here so a popup-mode app can never get
/// inline ghost text painted at an untrustworthy caret.
///
/// Deliberately does NOT call `OverlayWindow.hide()` — that clears the pending
/// completion in KeyCapture, which would break Tab for callers (word-by-word) that
/// keep the completion alive across a re-present. Visual dismissal only.
enum OverlayRouter {
    static func present(
        text: String,
        caretX: CGFloat,
        caretTopY: CGFloat,
        caretHeight: CGFloat,
        fontSize: CGFloat,
        inputFrame: CGRect?,
        bundleId: String
    ) {
        let caretPoint = NSPoint(x: caretX, y: caretTopY - caretHeight / 2)
        if PopupCardWindow.shouldUsePopup(
            bundleId: bundleId, caretHeight: caretHeight,
            caretPoint: caretPoint, inputFrame: inputFrame
        ), let frame = inputFrame {
            OverlayWindow.shared.orderOutOnly()
            PopupCardWindow.shared.show(text: text, inputFrame: frame)
        } else {
            PopupCardWindow.shared.hide()
            OverlayWindow.shared.show(
                text: text, x: caretX, y: caretTopY, caretHeight: caretHeight,
                fontSize: fontSize, inputFrame: inputFrame
            )
        }
    }
}
