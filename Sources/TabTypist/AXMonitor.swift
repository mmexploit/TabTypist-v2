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

        // Get the caret bounds
        var boundsValue: AnyObject?
        var caretRange = CFRangeMake(caretPos, 0)
        if let axRange = AXValueCreate(.cfRange, &caretRange) {
            AXUIElementCopyParameterizedAttributeValue(
                axElement,
                kAXBoundsForRangeParameterizedAttribute as CFString,
                axRange,
                &boundsValue
            )
        }

        var caretRect = CGRect.zero
        if let bv = boundsValue {
            AXValueGetValue(bv as! AXValue, .cgRect, &caretRect)
        }

        // AX coords: origin top-left, y increases down.
        // Cocoa coords: origin bottom-left, y increases up.
        // We report the TOP of the caret in Cocoa coords so OverlayWindow can
        // compute the dropdown position as: y - caretHeight - panelHeight - gap.
        let screenHeight = NSScreen.main?.frame.height ?? 0
        let screenY = screenHeight - caretRect.origin.y   // top of caret in Cocoa

        // Only report if prefix changed or app changed
        if prefix == lastPrefix && bundleId == lastBundleId { return }
        lastPrefix = prefix
        lastBundleId = bundleId

        IPCBridge.shared.notify(method: "contextUpdate", params: [
            "prefix": prefix,
            "suffix": suffix,
            "caretX": caretRect.origin.x,
            "caretY": screenY,
            "caretHeight": max(caretRect.height, 16.0),
            "appBundleId": bundleId,
            "isSecureField": isSecure,
        ])
    }
}
