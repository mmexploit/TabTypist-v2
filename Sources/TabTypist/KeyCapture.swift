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
    private var pendingCompletionText: String = ""

    func setCompletion(_ text: String) {
        pendingCompletionText = text
        completionIsVisible = !text.isEmpty
    }

    func clearCompletion() {
        pendingCompletionText = ""
        completionIsVisible = false
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
                let text = pendingCompletionText
                clearCompletion()
                // Do NOT post the synthetic Cmd+V from inside this tap callback:
                // posting re-enters our own tap and the events are often dropped
                // ("Tab consumed but nothing inserted"). Returning nil consumes
                // the Tab; the paste runs on the main run loop where it's reliable.
                DispatchQueue.main.async {
                    OverlayWindow.shared.hide()
                    self.insertCompletion(text)
                    IPCBridge.shared.notify(method: "acceptCompletion", params: [:])
                }
                return nil // consume the Tab
            }
            return Unmanaged.passRetained(event)

        case Int64(kVK_Escape):
            if completionIsVisible {
                clearCompletion()
                DispatchQueue.main.async { OverlayWindow.shared.hide() }
                IPCBridge.shared.notify(method: "dismissCompletion", params: [:])
            }
            return Unmanaged.passRetained(event)

        default:
            return Unmanaged.passRetained(event)
        }
    }

    // Insert completion by pasting via Cmd+V — works universally (terminal, browser, native apps).
    // AXUIElementSetAttributeValue fails in most modern apps.
    private func insertCompletion(_ text: String) {
        guard !text.isEmpty else { return }

        let pb = NSPasteboard.general
        let prev = pb.string(forType: .string)

        pb.clearContents()
        pb.setString(text, forType: .string)

        let src = CGEventSource(stateID: .hidSystemState)
        let vDown = CGEvent(keyboardEventSource: src, virtualKey: 0x09, keyDown: true)  // 0x09 = V
        let vUp   = CGEvent(keyboardEventSource: src, virtualKey: 0x09, keyDown: false)
        vDown?.flags = .maskCommand
        vUp?.flags   = .maskCommand
        vDown?.post(tap: .cghidEventTap)
        vUp?.post(tap: .cghidEventTap)

        // Restore previous pasteboard content after the paste completes.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15) {
            pb.clearContents()
            if let prev { pb.setString(prev, forType: .string) }
        }
    }
}
