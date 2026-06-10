import AppKit

// A small non-interactive-looking NSPanel anchored to the LEFT edge of the focused
// text field, showing TabTypist's "T" logo to indicate it is active. Unlike a plain
// marker it is clickable: a click opens Settings, mirroring the menu-bar item. It is
// a non-activating panel, so clicking it does not steal first-responder status from
// the user's text field until Settings is explicitly opened.
final class FieldEdgeIndicator: NSPanel {
    override var canBecomeKey: Bool  { false }
    override var canBecomeMain: Bool { false }

    static let shared = FieldEdgeIndicator()

    private static let iconSize: CGFloat = 18

    private let button: NSButton

    private init() {
        // Same "T" mark as the menu-bar status item (t.square.fill), so the active
        // affordance reads as the TabTypist brand rather than a generic glyph.
        let config = NSImage.SymbolConfiguration(pointSize: 14, weight: .semibold)
        let image = NSImage(systemSymbolName: "t.square.fill", accessibilityDescription: "TabTypist — open settings")?
            .withSymbolConfiguration(config)

        button = NSButton(image: image ?? NSImage(), target: nil, action: nil)
        button.isBordered = false
        button.bezelStyle = .regularSquare
        button.imagePosition = .imageOnly
        button.imageScaling = .scaleProportionallyDown
        button.contentTintColor = .controlAccentColor
        button.toolTip = "TabTypist is active — click for settings"

        super.init(
            contentRect: NSRect(x: 0, y: 0, width: Self.iconSize, height: Self.iconSize),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )

        level = NSWindow.Level(rawValue: Int(CGWindowLevelForKey(.floatingWindow)) + 1)
        isOpaque = false
        backgroundColor = .clear
        collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        ignoresMouseEvents = false   // must receive clicks
        hasShadow = false
        contentView = button

        button.target = self
        button.action = #selector(openSettings)
    }

    @objc private func openSettings() {
        SettingsWindowController.shared.show()
    }

    func show(inputFrame: CGRect) {
        guard inputFrame.width > 10 && inputFrame.height > 10 else { hide(); return }
        let size = Self.iconSize
        let gap: CGFloat = 4

        // Anchor just outside the field's LEFT edge, vertically centered. If the field
        // hugs the screen's left edge (no room outside), tuck the logo just inside so
        // it stays visible instead of clipping off-screen.
        var fx = inputFrame.minX - size - gap
        if fx < 2 { fx = inputFrame.minX + gap }
        let fy = inputFrame.midY - size / 2

        setFrame(NSRect(x: fx, y: fy, width: size, height: size), display: true)
        contentView?.frame = NSRect(origin: .zero, size: frame.size)
        alphaValue = 1
        orderFront(nil)
    }

    func hide() {
        orderOut(nil)
    }
}
