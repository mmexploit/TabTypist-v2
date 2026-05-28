import ApplicationServices
import AppKit
import Foundation

// Reports caret-rect + text-context changes from the focused text field.
final class AXMonitor: @unchecked Sendable {
    static let shared = AXMonitor()

    private var pollTimer: Timer?
    private var lastBundleId: String = ""
    private var lastPrefix: String = ""

    func start() {
        pollTimer = Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { [weak self] _ in
            self?.poll()
        }
    }

    func stop() {
        pollTimer?.invalidate()
        pollTimer = nil
    }

    private func poll() {
        guard let app = NSWorkspace.shared.frontmostApplication else { return }
        let bundleId = app.bundleIdentifier ?? "unknown"
        let pid = app.processIdentifier

        // Hide overlay immediately when the user switches to a different app.
        if bundleId != lastBundleId && !lastBundleId.isEmpty {
            DispatchQueue.main.async { OverlayWindow.shared.hide() }
        }

        let appRef = AXUIElementCreateApplication(pid)
        var focusedElement: AnyObject?
        let result = AXUIElementCopyAttributeValue(
            appRef, kAXFocusedUIElementAttribute as CFString, &focusedElement
        )
        guard result == .success, let element = focusedElement else { return }
        let axElement = element as! AXUIElement

        // Check if it's a secure/password field
        var isSecure = false
        var secureValue: AnyObject?
        if AXUIElementCopyAttributeValue(axElement, "AXIsPasswordField" as CFString, &secureValue) == .success {
            isSecure = (secureValue as? Bool) ?? false
        }

        // Get the full text value
        var textValue: AnyObject?
        guard AXUIElementCopyAttributeValue(axElement, kAXValueAttribute as CFString, &textValue) == .success,
              let fullText = textValue as? String
        else { return }

        // Get the selected range to find caret position
        var rangeValue: AnyObject?
        guard AXUIElementCopyAttributeValue(
            axElement, kAXSelectedTextRangeAttribute as CFString, &rangeValue
        ) == .success else { return }

        var cfRange = CFRange()
        guard let rangeVal = rangeValue,
              AXValueGetValue(rangeVal as! AXValue, .cfRange, &cfRange)
        else { return }

        let caretPos = cfRange.location
        let prefix = String(fullText.prefix(caretPos))
        let suffix = String(fullText.dropFirst(caretPos))

        // Get caret bounds.
        //
        // A zero-length range query (caretPos, 0) often returns the wrong position in
        // rich-text views like Notes NSTextView: it returns the bounds of the newline
        // character at the end of the PREVIOUS paragraph rather than the cursor on the
        // CURRENT line.
        //
        // More reliable: query the last non-newline character before the cursor with
        // length=1, then use x + charWidth for the caret x and the same y.
        // Fall back to the zero-length query if there is no such character.
        var caretRect = CGRect.zero
        var useCharOffset: CGFloat = 0  // added to rect.origin.x + rect.width to get cursor x

        let prevCharIdx = lastNonNewlineCharIndex(in: prefix, caretPos: caretPos)
        if let idx = prevCharIdx {
            var charRange = CFRangeMake(idx, 1)
            var bv: AnyObject?
            if let axRange = AXValueCreate(.cfRange, &charRange) {
                AXUIElementCopyParameterizedAttributeValue(
                    axElement, kAXBoundsForRangeParameterizedAttribute as CFString, axRange, &bv
                )
            }
            if let bv, AXValueGetValue(bv as! AXValue, .cgRect, &caretRect), caretRect.height > 0 {
                // Cursor x = right edge of the last character
                useCharOffset = caretRect.width
            } else {
                caretRect = .zero
            }
        }

        // Fall back to zero-length range if the character-based query failed
        if caretRect.height == 0 {
            var zeroRange = CFRangeMake(caretPos, 0)
            var bv: AnyObject?
            if let axRange = AXValueCreate(.cfRange, &zeroRange) {
                AXUIElementCopyParameterizedAttributeValue(
                    axElement, kAXBoundsForRangeParameterizedAttribute as CFString, axRange, &bv
                )
            }
            if let bv { AXValueGetValue(bv as! AXValue, .cgRect, &caretRect) }
            useCharOffset = 0
        }

        // AX coords: origin top-left of PRIMARY (menu-bar) display, y increases down.
        // Cocoa coords: origin bottom-left of primary display, y increases up.
        // MUST use NSScreen.screens.first (primary/menu-bar display), not NSScreen.main
        // (focused display) — they differ on multi-monitor setups, breaking the math.
        let primaryScreenHeight = NSScreen.screens.first?.frame.height ?? NSScreen.main?.frame.height ?? 0
        let caretX   = caretRect.origin.x + useCharOffset
        let screenY  = primaryScreenHeight - caretRect.origin.y   // top of caret in Cocoa

        fputs("AXMonitor: primaryH=\(primaryScreenHeight) axRect=\(caretRect) caretX=\(caretX) screenY=\(screenY) bundle=\(bundleId)\n", stderr)

        // Only report if prefix changed or app changed
        if prefix == lastPrefix && bundleId == lastBundleId { return }
        lastPrefix = prefix
        lastBundleId = bundleId

        // caretHeight=0 means AX couldn't determine caret bounds (Electron, terminal, etc.).
        // Send caretHeight=0 as a sentinel so Rust skips showOverlay for those apps.
        // Cast CGFloat → Double explicitly: AnyCodable only encodes Double,
        // not CGFloat (a distinct Swift struct), so CGFloat values serialize as null.
        IPCBridge.shared.notify(method: "contextUpdate", params: [
            "prefix":        prefix,
            "suffix":        suffix,
            "caretX":        Double(caretX),
            "caretY":        Double(screenY),
            "caretHeight":   Double(caretRect.height),   // 0 = no valid caret bounds
            "appBundleId":   bundleId,
            "isSecureField": isSecure,
        ])
    }

    // Returns the string index of the last character before caretPos that is not a newline.
    // Returns nil if the prefix is empty or consists only of newlines.
    private func lastNonNewlineCharIndex(in prefix: String, caretPos: Int) -> Int? {
        guard caretPos > 0 else { return nil }
        var idx = caretPos - 1
        let chars = Array(prefix.unicodeScalars)
        while idx >= 0 {
            if chars[idx].value != 0x0A && chars[idx].value != 0x0D { return idx }
            idx -= 1
        }
        return nil
    }
}
