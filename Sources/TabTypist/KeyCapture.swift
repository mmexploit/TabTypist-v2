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
    private(set) var completionIsVisible: Bool = false
    private var pendingCompletionText: String = ""

    func setCompletion(_ text: String) {
        pendingCompletionText = text
        completionIsVisible = !text.isEmpty
        fputs("KeyCapture: setCompletion visible=\(completionIsVisible) len=\(text.count) text=\"\(text.prefix(40))\"\n", stderr)
    }

    func clearCompletion() {
        if completionIsVisible {
            fputs("KeyCapture: clearCompletion (was visible)\n", stderr)
        }
        pendingCompletionText = ""
        completionIsVisible = false
    }

    func start() {
        // Check Input Monitoring permission BEFORE creating the tap. macOS will
        // happily return a non-nil tap even when permission is revoked (typical
        // after a rebuild changes the cdhash for an ad-hoc signed binary), but
        // it silently drops every event before the callback ever fires. Without
        // this check the only visible symptom is "Tab does nothing", which is
        // indistinguishable from a bug in the completion pipeline.
        let access = IOHIDCheckAccess(kIOHIDRequestTypeListenEvent)
        switch access {
        case kIOHIDAccessTypeGranted:
            fputs("KeyCapture: Input Monitoring permission GRANTED\n", stderr)
        case kIOHIDAccessTypeDenied:
            fputs("KeyCapture: ⚠️ Input Monitoring permission DENIED — Tab will not be intercepted.\n  Fix: System Settings → Privacy & Security → Input Monitoring → remove TabTypist (if listed) and re-add /Users/mubarekendrie/Personal/TabTypist/dist/TabTypist.app\n", stderr)
            _ = IOHIDRequestAccess(kIOHIDRequestTypeListenEvent)
        default:
            fputs("KeyCapture: Input Monitoring permission UNKNOWN — prompting\n", stderr)
            _ = IOHIDRequestAccess(kIOHIDRequestTypeListenEvent)
        }

        let mask: CGEventMask = (1 << CGEventType.keyDown.rawValue)

        let selfPtr = Unmanaged.passUnretained(self).toOpaque()
        // Tap at the HID level instead of session level. Both require the same
        // permissions, but cghidEventTap sits one layer closer to hardware and
        // is less likely to be starved by another headInsert tap from e.g.
        // Karabiner or Cursor's helper apps.
        eventTap = CGEvent.tapCreate(
            tap: .cghidEventTap,
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

        if let tap = eventTap {
            let runLoopSource = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
            CFRunLoopAddSource(CFRunLoopGetMain(), runLoopSource, .commonModes)
            CGEvent.tapEnable(tap: tap, enable: true)
            let isEnabled = CGEvent.tapIsEnabled(tap: tap)
            let axTrusted = AXIsProcessTrusted()
            fputs("KeyCapture: tap created (cghidEventTap), enabled=\(isEnabled), AXIsProcessTrusted=\(axTrusted)\n", stderr)

            // Health-check: every 3 s, verify the tap is still enabled and log
            // a heartbeat. macOS can disable the tap silently (without sending
            // a tapDisabledBy* event) in certain conditions; without periodic
            // re-enable we'd go silent forever.
            healthTimer = Timer.scheduledTimer(withTimeInterval: 3.0, repeats: true) { [weak self] _ in
                guard let self, let tap = self.eventTap else { return }
                let enabled = CGEvent.tapIsEnabled(tap: tap)
                fputs("KeyCapture: health check enabled=\(enabled)\n", stderr)
                if !enabled {
                    fputs("KeyCapture: tap was silently disabled — re-enabling\n", stderr)
                    CGEvent.tapEnable(tap: tap, enable: true)
                }
            }
        } else {
            fputs("TabTypist: failed to create CGEventTap — Input Monitoring permission needed\n", stderr)
        }
    }

    private func handleEvent(_ event: CGEvent) -> Unmanaged<CGEvent>? {
        // macOS disables the tap if our callback is too slow or the user moves
        // around. Both events arrive regardless of the eventsOfInterest mask;
        // we must re-enable or we go silent forever.
        let type = event.type
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            fputs("KeyCapture: tap disabled (type=\(type.rawValue)) — re-enabling\n", stderr)
            if let tap = eventTap { CGEvent.tapEnable(tap: tap, enable: true) }
            return Unmanaged.passUnretained(event)
        }

        let keyCode = event.getIntegerValueField(.keyboardEventKeycode)
        // Verbose: prove the tap is alive. If you press a key and DON'T see this
        // line in /tmp/tt.log, the tap isn't being fed events — that's a system
        // routing issue (another tap ahead of ours, or permission silently revoked).
        fputs("KeyCapture: rx keyCode=\(keyCode) type=\(type.rawValue)\n", stderr)

        switch keyCode {
        case Int64(kVK_Tab):
            fputs("KeyCapture: Tab pressed, visible=\(completionIsVisible) len=\(pendingCompletionText.count)\n", stderr)
            if completionIsVisible {
                let text = pendingCompletionText
                clearCompletion()
                // Hide overlay immediately and insert text via paste (works in all apps including CLI).
                DispatchQueue.main.async { OverlayWindow.shared.hide() }
                insertCompletion(text)
                IPCBridge.shared.notify(method: "acceptCompletion", params: [:])
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
        let pb = NSPasteboard.general
        let prev = pb.string(forType: .string)

        pb.clearContents()
        pb.setString(text, forType: .string)

        let src = CGEventSource(stateID: .hidSystemState)
        let vDown = CGEvent(keyboardEventSource: src, virtualKey: 0x09, keyDown: true)
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
