import ApplicationServices
import AppKit
import Carbon.HIToolbox
import Foundation

// CGEventTap for Tab and Escape.  Tab accepts the current completion; Escape dismisses it.
final class KeyCapture: @unchecked Sendable {
    static let shared = KeyCapture()

    private var eventTap: CFMachPort?
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
        let mask: CGEventMask = (1 << CGEventType.keyDown.rawValue)

        let selfPtr = Unmanaged.passUnretained(self).toOpaque()
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

        if let tap = eventTap {
            let runLoopSource = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
            CFRunLoopAddSource(CFRunLoopGetMain(), runLoopSource, .commonModes)
            CGEvent.tapEnable(tap: tap, enable: true)
        } else {
            fputs("TabTypist: failed to create CGEventTap — Input Monitoring permission needed\n", stderr)
        }
    }

    private func handleEvent(_ event: CGEvent) -> Unmanaged<CGEvent>? {
        let keyCode = event.getIntegerValueField(.keyboardEventKeycode)

        switch keyCode {
        case Int64(kVK_Tab):
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
