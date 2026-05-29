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

        // Hide the overlay immediately on any real app activation. The poll loop only hides
        // after the new app successfully returns AX text data — if you navigate to the
        // desktop or any app with no focused text field the early-return guards fire first
        // and the overlay stays up. This notification fires for every genuine foreground
        // transition (unlike frontmostApplication polling, it is not affected by invisible
        // helper processes briefly grabbing the frontmost slot).
        NSWorkspace.shared.notificationCenter.addObserver(
            self,
            selector: #selector(appDidActivate(_:)),
            name: NSWorkspace.didActivateApplicationNotification,
            object: nil
        )
    }

    @objc private func appDidActivate(_ note: Notification) {
        guard let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication,
              let bundleId = app.bundleIdentifier else { return }
        let ourBundleId = Bundle.main.bundleIdentifier ?? ""
        guard bundleId != ourBundleId else { return }

        if !lastBundleId.isEmpty && bundleId != lastBundleId {
            DispatchQueue.main.async { OverlayWindow.shared.hide() }
        }
        // Update state so the poll loop agrees on the current app and won't double-hide.
        lastBundleId = bundleId
        lastPrefix = ""
    }

    func stop() {
        pollTimer?.invalidate()
        pollTimer = nil
        NSWorkspace.shared.notificationCenter.removeObserver(self)
    }

    private func poll() {
        guard let app = NSWorkspace.shared.frontmostApplication else { return }
        let bundleId = app.bundleIdentifier ?? "unknown"
        let pid = app.processIdentifier

        // If macOS briefly reports US as the frontmost app (this can happen when our
        // non-activating overlay panel is ordered front), bail out of the entire poll.
        // Otherwise the bundle-id branch below fires OverlayWindow.hide(), which
        // clears the completion state and makes Tab fall through on the next keystroke.
        let ourBundleId = Bundle.main.bundleIdentifier ?? ""
        if !ourBundleId.isEmpty && bundleId == ourBundleId { return }

        // Note: we intentionally do NOT hide-on-bundle-change here yet. Apps like
        // Cursor briefly grab `frontmostApplication` while staying invisible, and
        // their AX layer returns degenerate fields/carets. We defer the hide
        // decision until we've validated that the new app actually exposes a real
        // editing context (see the degenerate-AX check further down).

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

        // Font size of the focused element. Many apps report this via kAXFontAttribute;
        // fall back to 0 so the overlay can use its caretHeight-based estimate instead.
        var axFontSize: CGFloat = 0
        var fontAttr: AnyObject?
        if AXUIElementCopyAttributeValue(axElement, "AXFont" as CFString, &fontAttr) == .success,
           let fontDict = fontAttr as? [String: Any] {
            if let sz = fontDict["AXFontSize"] as? CGFloat { axFontSize = sz }
            else if let sz = fontDict["AXFontSize"] as? Double { axFontSize = CGFloat(sz) }
        }

        // Input-field frame in Cocoa coords. Used downstream to clamp the overlay so it
        // can't render past the edge of the host text view. Zero = unavailable.
        var inputFrameAX = CGRect.zero
        var frameVal: AnyObject?
        if AXUIElementCopyAttributeValue(axElement, "AXFrame" as CFString, &frameVal) == .success,
           let fv = frameVal {
            AXValueGetValue(fv as! AXValue, .cgRect, &inputFrameAX)
        }
        let inputX = inputFrameAX.origin.x
        let inputY = inputFrameAX.height > 0
            ? primaryScreenHeight - inputFrameAX.origin.y - inputFrameAX.height
            : 0
        let inputW = inputFrameAX.width
        let inputH = inputFrameAX.height

        // Degenerate-AX guard. When an app briefly grabs `frontmostApplication`
        // without a real focused text field (Cursor's ToDesktop helper, Slack
        // notification windows, etc.), AX returns junk values like a 0×0 caret
        // inside a 1×1 field. Letting that data flow further down would (a) clear
        // the active completion via the prefix-change hide and (b) update
        // lastPrefix/lastBundleId to garbage, so the next real poll from the
        // user's actual app looks like an app switch *back*. Skip without
        // touching state — the next real poll restores the previous flow.
        if caretRect.height == 0 && inputW < 10 && inputH < 10 { return }

        // Now that we've confirmed the new app exposes a real editing context,
        // a true bundle change is a genuine app switch — hide the stale overlay.
        if bundleId != lastBundleId && !lastBundleId.isEmpty {
            DispatchQueue.main.async { OverlayWindow.shared.hide() }
        }

        // Only report if prefix changed or app changed
        if prefix == lastPrefix && bundleId == lastBundleId { return }

        // Word-by-word partial accept: the user just accepted one word via Tab.
        // Instead of hiding the overlay and waiting for new inference (250ms+ gap),
        // reposition the remaining ghost text at the updated caret and skip the
        // contextUpdate so Rust doesn't re-infer. The flag is consumed exactly once.
        if bundleId == lastBundleId && KeyCapture.shared.isWordByWordInProgress {
            KeyCapture.shared.clearWordByWordFlag()
            lastPrefix = prefix
            let remaining = KeyCapture.shared.pendingCompletionText
            if !remaining.isEmpty && caretRect.height > 0 {
                let frame: CGRect? = inputFrameAX.height > 0
                    ? CGRect(x: inputX, y: inputY, width: inputW, height: inputH)
                    : nil
                DispatchQueue.main.async {
                    OverlayWindow.shared.show(
                        text: remaining, x: caretX, y: screenY,
                        caretHeight: caretRect.height, fontSize: axFontSize,
                        inputFrame: frame)
                }
            }
            return
        }

        lastPrefix = prefix
        lastBundleId = bundleId

        // Log once per real contextUpdate, not 20x/sec on idle polls.
        fputs("AXMonitor: primaryH=\(primaryScreenHeight) axRect=\(caretRect) caretX=\(caretX) screenY=\(screenY) field=(\(inputX),\(inputY),\(inputW),\(inputH)) bundle=\(bundleId)\n", stderr)

        // User actually typed something — the currently-visible completion (if any)
        // was offered for an older prefix and would visually sit on top of the new
        // input. Hide it now; the new overlay arrives after the Rust debounce +
        // inference finishes for this latest prefix. Done client-side so there's no
        // IPC round-trip and no Rust handler clearing completion state behind our back.
        DispatchQueue.main.async { OverlayWindow.shared.hide() }

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
            "fontSize":      Double(axFontSize),          // 0 = unavailable
            "inputFrameX":   Double(inputX),
            "inputFrameY":   Double(inputY),
            "inputFrameW":   Double(inputW),
            "inputFrameH":   Double(inputH),
            "appBundleId":   bundleId,
            "isSecureField": isSecure,
        ])
    }

    // Returns the index (into `chars`) of the last character before the caret that is
    // not a newline. Returns nil if the prefix is empty or consists only of newlines.
    //
    // Index discipline: AX's caretPos is in UTF-16 code units, the Swift String prefix
    // we receive was constructed by Character count, and unicodeScalars iterate Unicode
    // scalars. Those three are only equal for pure-ASCII text. Rather than try to
    // re-index across systems, we just walk back from the end of `chars` — that's
    // safe regardless of how prefix was built, and the result is used by the caller
    // only to query 1-char AX bounds, which is itself a fuzzy heuristic.
    private func lastNonNewlineCharIndex(in prefix: String, caretPos: Int) -> Int? {
        let chars = Array(prefix.unicodeScalars)
        guard !chars.isEmpty else { return nil }
        var idx = chars.count - 1
        while idx >= 0 {
            let v = chars[idx].value
            if v != 0x0A && v != 0x0D { return idx }
            idx -= 1
        }
        return nil
    }
}
