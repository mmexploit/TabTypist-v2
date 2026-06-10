import ApplicationServices
import AppKit
import Carbon.HIToolbox
import Foundation
import IOKit.hid

// CGEventTap for Tab and Escape.  Tab accepts the current completion; Escape dismisses it.
final class KeyCapture: @unchecked Sendable {
    static let shared = KeyCapture()

    private var eventTap: CFMachPort?
    private var healthTimer: Timer?
    private var retryTimer: Timer?
    private(set) var completionIsVisible: Bool = false
    private(set) var pendingCompletionText: String = ""

    // Set when the user accepted one word of a multi-word completion.
    // AXMonitor checks this flag to reposition the overlay instead of
    // hiding it and triggering new inference.
    private(set) var isWordByWordInProgress: Bool = false

    // Full-accept keycode: -1 = disabled.  Default: kVK_ANSI_Grave (50 = backtick).
    // Persisted in UserDefaults; change takes effect immediately without restart.
    private(set) var fullAcceptKeyCode: Int64 = {
        guard UserDefaults.standard.object(forKey: "fullAcceptKeyCode") != nil else {
            return Int64(kVK_ANSI_Grave)
        }
        return Int64(UserDefaults.standard.integer(forKey: "fullAcceptKeyCode"))
    }()

    func setFullAcceptKeyCode(_ keyCode: Int64) {
        fullAcceptKeyCode = keyCode
        UserDefaults.standard.set(Int(keyCode), forKey: "fullAcceptKeyCode")
    }

    func setCompletion(_ text: String) {
        pendingCompletionText = text
        completionIsVisible = !text.isEmpty
        isWordByWordInProgress = false
    }

    func clearCompletion() {
        pendingCompletionText = ""
        completionIsVisible = false
        isWordByWordInProgress = false
    }

    func clearWordByWordFlag() {
        isWordByWordInProgress = false
    }

    // autoAcceptTrailingPunctuation: when false, non-alphanumeric chars at the end of
    // the accepted word are left in `remaining` so the user can accept them separately.
    private func nextWord(from text: String) -> (word: String, remaining: String) {
        let autoAcceptPunct = UserDefaults.standard.object(forKey: "autoAcceptTrailingPunctuation")
            .flatMap { $0 as? Bool } ?? true

        guard !text.isEmpty else { return ("", "") }
        var idx = text.startIndex
        var seenNonSpace = false
        while idx < text.endIndex {
            let c = text[idx]
            if c == " " || c == "\t" {
                if seenNonSpace {
                    idx = text.index(after: idx)
                    break
                }
            } else {
                seenNonSpace = true
            }
            idx = text.index(after: idx)
        }
        var word = String(text[..<idx])
        var remaining = String(text[idx...])

        if !autoAcceptPunct {
            // Strip trailing punctuation from `word` back into `remaining`.
            // "Don't" and "U.S.A" are excluded: only strip the trailing run of
            // non-alphanumeric chars if there are alphanumeric chars before them.
            var splitAt = word.endIndex
            var cursor = word.endIndex
            while cursor > word.startIndex {
                let prev = word.index(before: cursor)
                if word[prev].isLetter || word[prev].isNumber { break }
                splitAt = prev
                cursor = prev
            }
            if splitAt < word.endIndex && splitAt > word.startIndex {
                remaining = String(word[splitAt...]) + remaining
                word = String(word[..<splitAt])
            }
        }

        return (word, remaining)
    }

    func start() {
        // A consuming (.defaultTap) CGEventTap needs TWO separate macOS grants:
        //   1. Input Monitoring — to *observe* key events.
        //   2. Accessibility    — to *consume/modify* them (a listen-only tap
        //      would need only #1, but we must swallow the Tab key).
        // Missing EITHER makes tapCreate return nil. Log both so the failure
        // names the missing grant instead of guessing.
        let access = IOHIDCheckAccess(kIOHIDRequestTypeListenEvent)
        switch access {
        case kIOHIDAccessTypeGranted:
            fputs("KeyCapture: Input Monitoring GRANTED\n", stderr)
        default:
            fputs("KeyCapture: ⚠️ Input Monitoring NOT granted — System Settings → Privacy & Security → Input Monitoring → enable TabTypist.\n", stderr)
            _ = IOHIDRequestAccess(kIOHIDRequestTypeListenEvent)
        }

        if AXIsProcessTrusted() {
            fputs("KeyCapture: Accessibility GRANTED\n", stderr)
        } else {
            fputs("KeyCapture: ⚠️ Accessibility NOT granted — a consuming tap can't be created. Prompting.\n", stderr)
            let opts = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true]
            _ = AXIsProcessTrustedWithOptions(opts as CFDictionary)
        }

        if !createTap() {
            // Both grants are user-toggled in System Settings and can be flipped
            // while we run. Poll every 2 s and create the tap the moment both are
            // present, so "enable the toggle, then it works" needs no relaunch.
            retryTimer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] timer in
                guard let self else { timer.invalidate(); return }
                if self.createTap() {
                    timer.invalidate()
                    self.retryTimer = nil
                }
            }
        }
    }

    /// Attempts to create + enable the event tap. Returns true on success.
    private func createTap() -> Bool {
        let mask: CGEventMask = (1 << CGEventType.keyDown.rawValue)
        let selfPtr = Unmanaged.passUnretained(self).toOpaque()

        // Session-level tap. A `.cghidEventTap` from a signed, non-root app can
        // report enabled=true yet deliver zero events; `.cgSessionEventTap` is
        // the conventional layer for an accessibility app that consumes keys.
        eventTap = CGEvent.tapCreate(
            tap: .cgSessionEventTap,
            place: .headInsertEventTap,
            options: .defaultTap,
            eventsOfInterest: mask,
            callback: { _, _, event, refcon -> Unmanaged<CGEvent>? in
                guard let refcon else { return Unmanaged.passRetained(event) }
                let capture = Unmanaged<KeyCapture>.fromOpaque(refcon).takeUnretainedValue()
                return capture.handleEvent(event)
            },
            userInfo: selfPtr
        )

        guard let tap = eventTap else {
            let im = IOHIDCheckAccess(kIOHIDRequestTypeListenEvent) == kIOHIDAccessTypeGranted
            fputs("KeyCapture: ⚠️ tapCreate failed. InputMonitoring=\(im) Accessibility=\(AXIsProcessTrusted())\n", stderr)
            return false
        }

        let runLoopSource = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
        CFRunLoopAddSource(CFRunLoopGetMain(), runLoopSource, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)

        // Secure input mode (EnableSecureEventInput, e.g. a focused password
        // field or pending keychain dialog) blocks EVERY event tap system-wide
        // while the focused app still receives keys — the exact "tap enabled but
        // zero events" symptom. Log it so that case is unambiguous.
        fputs("KeyCapture: tap created, enabled=\(CGEvent.tapIsEnabled(tap: tap)) SecureInputActive=\(IsSecureEventInputEnabled())\n", stderr)

        // Health-check: macOS can silently disable the tap; re-enable if so.
        healthTimer?.invalidate()
        healthTimer = Timer.scheduledTimer(withTimeInterval: 3.0, repeats: true) { [weak self] _ in
            guard let self, let tap = self.eventTap else { return }
            if !CGEvent.tapIsEnabled(tap: tap) {
                fputs("KeyCapture: tap silently disabled — re-enabling\n", stderr)
                CGEvent.tapEnable(tap: tap, enable: true)
            }
        }
        return true
    }

    private func handleEvent(_ event: CGEvent) -> Unmanaged<CGEvent>? {
        // macOS disables the tap if our callback is too slow or the user moves
        // around; we must re-enable or we go silent forever.
        let type = event.type
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            if let tap = eventTap { CGEvent.tapEnable(tap: tap, enable: true) }
            return Unmanaged.passUnretained(event)
        }

        let keyCode = event.getIntegerValueField(.keyboardEventKeycode)

        switch keyCode {
        case Int64(kVK_Tab):
            if completionIsVisible {
                let (word, rest) = nextWord(from: pendingCompletionText)
                // Do NOT post Cmd+V from inside the tap callback — posting re-enters
                // our tap and events are often dropped. Return nil to consume the Tab;
                // the paste runs on the main run loop where it is reliable.
                if rest.isEmpty {
                    // Last (or only) word — full acceptance.
                    clearCompletion()
                    DispatchQueue.main.async {
                        OverlayWindow.shared.hide(armStabilityGate: true)
                        self.insertCompletion(word)
                        IPCBridge.shared.notify(method: "acceptCompletion", params: [:])
                    }
                } else {
                    // More words remain — partial acceptance ( style).
                    // Set the flag so the next AXMonitor poll repositions the overlay
                    // with the remaining text instead of hiding and re-inferring.
                    pendingCompletionText = rest
                    isWordByWordInProgress = true
                    DispatchQueue.main.async {
                        self.insertCompletion(word)
                    }
                }
                return nil // consume the Tab in both cases
            }
            return Unmanaged.passRetained(event)

        case Int64(kVK_Escape):
            if completionIsVisible {
                clearCompletion()
                DispatchQueue.main.async { OverlayWindow.shared.hide(armStabilityGate: true) }
                IPCBridge.shared.notify(method: "dismissCompletion", params: [:])
            }
            return Unmanaged.passRetained(event)

        case _ where fullAcceptKeyCode > 0 && keyCode == fullAcceptKeyCode:
            if completionIsVisible {
                let all = pendingCompletionText
                clearCompletion()
                DispatchQueue.main.async {
                    OverlayWindow.shared.hide(armStabilityGate: true)
                    self.insertCompletion(all)
                    IPCBridge.shared.notify(method: "acceptCompletion", params: [:])
                }
                return nil // consume the key
            }
            return Unmanaged.passRetained(event)

        default:
            return Unmanaged.passRetained(event)
        }
    }

    // ── Text injection ────────────────────────────────────────────────────────

    private enum InjectionMethod: String { case ax, cmdV }

    private func cachedInjectionMethod(bundleId: String) -> InjectionMethod? {
        guard let raw = UserDefaults.standard.string(forKey: "injectionMethod.\(bundleId)") else { return nil }
        return InjectionMethod(rawValue: raw)
    }

    private func cacheInjectionMethod(_ method: InjectionMethod, bundleId: String) {
        UserDefaults.standard.set(method.rawValue, forKey: "injectionMethod.\(bundleId)")
    }

    /// Try AX set-value first; fall back to Cmd+V if the app rejects it.
    /// Per-bundle-ID preference is cached in UserDefaults.
    private func insertCompletion(_ text: String) {
        guard !text.isEmpty else { return }
        let bundleId = NSWorkspace.shared.frontmostApplication?.bundleIdentifier ?? ""
        let cached = cachedInjectionMethod(bundleId: bundleId)

        if cached != .cmdV {
            if tryAXInsert(text) {
                if cached == nil { cacheInjectionMethod(.ax, bundleId: bundleId) }
                return
            }
            if cached == .ax {
                // Previously worked but now failing — invalidate cache and fall through.
                UserDefaults.standard.removeObject(forKey: "injectionMethod.\(bundleId)")
            }
        }

        cacheInjectionMethod(.cmdV, bundleId: bundleId)
        cmdVInsert(text)
    }

    /// Attempt insertion via AX. Returns false if the app rejects it.
    @discardableResult
    private func tryAXInsert(_ text: String) -> Bool {
        let systemWide = AXUIElementCreateSystemWide()
        var focusedRef: CFTypeRef?
        guard AXUIElementCopyAttributeValue(
            systemWide, kAXFocusedUIElementAttribute as CFString, &focusedRef
        ) == .success, let focusedRef else { return false }

        let element = focusedRef as! AXUIElement  // safe: AX always returns AXUIElement here

        // Preferred: insert at the caret by replacing the (empty) selection. This is a
        // localized edit, so rich-text engines like Notes' NSTextView keep their
        // formatting AND don't re-run a document-wide "Capitalize Words" substitution
        // on the surrounding text. The whole-value rewrite below was handing Notes a
        // brand-new document string, which made it Title-Case every accepted word.
        if AXUIElementSetAttributeValue(
            element, kAXSelectedTextAttribute as CFString, text as CFString
        ) == .success {
            return true
        }

        // Fallback: whole-value rewrite for fields that don't accept AXSelectedText
        // writes (some custom/web text fields). Caret is recomputed from the selection.
        var valueRef: CFTypeRef?
        guard AXUIElementCopyAttributeValue(
            element, kAXValueAttribute as CFString, &valueRef
        ) == .success, let currentStr = valueRef as? String else { return false }

        var rangeRef: CFTypeRef?
        guard AXUIElementCopyAttributeValue(
            element, kAXSelectedTextRangeAttribute as CFString, &rangeRef
        ) == .success, let rangeRef else { return false }

        var range = CFRange()
        guard AXValueGetValue(rangeRef as! AXValue, .cfRange, &range) else { return false }
        let insertAt = range.location + range.length  // end of selection = caret

        let ns = currentStr as NSString
        guard insertAt >= 0 && insertAt <= ns.length else { return false }

        let newStr = ns.substring(to: insertAt) + text + ns.substring(from: insertAt)
        guard AXUIElementSetAttributeValue(
            element, kAXValueAttribute as CFString, newStr as CFString
        ) == .success else { return false }

        // Advance caret to end of inserted text.
        var newRange = CFRange(location: insertAt + (text as NSString).length, length: 0)
        if let axRange = AXValueCreate(.cfRange, &newRange) {
            AXUIElementSetAttributeValue(element, kAXSelectedTextRangeAttribute as CFString, axRange)
        }
        return true
    }

    /// Paste via Cmd+V. Restores the previous pasteboard content after 150 ms.
    private func cmdVInsert(_ text: String) {
        let pb = NSPasteboard.general
        let prev = pb.string(forType: .string)

        pb.clearContents()
        pb.setString(text, forType: .string)

        let src = CGEventSource(stateID: .hidSystemState)
        let vDown = CGEvent(keyboardEventSource: src, virtualKey: 0x09, keyDown: true)   // V
        let vUp   = CGEvent(keyboardEventSource: src, virtualKey: 0x09, keyDown: false)
        vDown?.flags = .maskCommand
        vUp?.flags   = .maskCommand
        vDown?.post(tap: .cghidEventTap)
        vUp?.post(tap: .cghidEventTap)

        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15) {
            pb.clearContents()
            if let prev { pb.setString(prev, forType: .string) }
        }
    }
}
