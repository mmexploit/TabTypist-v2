import ApplicationServices
import AppKit
import Foundation

// Reports caret-rect + text-context changes from the focused text field.
final class AXMonitor: @unchecked Sendable {
    static let shared = AXMonitor()

    private var pollTimer: Timer?
    private var lastBundleId: String = ""
    private var lastPrefix: String = ""

    // AXObserver — registered on the current app element; nil when not watching.
    private var axObserver: AXObserver?
    private var observedPid: pid_t = 0

    // Latest OCR result (refreshed asynchronously on each context change), tagged with
    // the bundle id it was captured from. The tag scopes the context to its source app:
    // OCR only updates `latestVisualContext` when capture SUCCEEDS, so without the tag a
    // failed capture in a new app (e.g. Notes, where the text field fills the window and
    // there's nothing above it) would leave the previous app's context (e.g. Telegram)
    // attached to the new app's completions.
    private var latestVisualContext: String = ""
    private var latestVisualContextBundle: String = ""
    // OCR is expensive (ScreenCaptureKit + Vision). Capture once per focused field,
    // then refresh only while the user is actively typing AND the cached excerpt has
    // aged past `ocrRefreshInterval` — in a live chat, new messages arrive while you
    // compose, and a focus-time-only snapshot goes stale (the message being replied to
    // scrolls away; the prompt keeps describing the previous screen). The age gate
    // keeps this far from the old every-2s re-capture that churned memory: worst case
    // is a handful of captures per minute, and an idle field re-captures nothing.
    // `pendingOCRWork` enforces a short focus-settle window so a flapping focus
    // (Electron/Chromium lose+re-acquire the AX element) runs the pipeline once it's
    // stable, not once per flap. `ocrInFlight` still prevents overlapping captures.
    private var lastOCRFieldKey: String = ""
    private var pendingOCRWork: DispatchWorkItem?
    private var ocrInFlight: Bool = false
    private var lastOCRCompletedAt: Date = .distantPast
    private static let ocrSettleInterval: TimeInterval = 0.25  // 250 ms settle after focus change
    private static let ocrRefreshInterval: TimeInterval = 15

    // Adaptive backoff: start at 80 ms, double after 5 unchanged polls, cap at 200 ms.
    private var currentPollInterval: TimeInterval = 0.08
    private var unchangedPollCount: Int = 0

    // Debounce for Apple Intelligence completions (mirrors the 20 ms Rust debounce).
    private var aiDebounceWork: DispatchWorkItem?

    // Rolling average of single-character AX frame widths — used for post-accept
    // caret position prediction while the real AX update is still in-flight.
    private var charWidthSamples: [CGFloat] = []
    private var lastFontSize: CGFloat = 0
    private static let maxCharWidthSamples = 10

    var avgCharWidth: CGFloat {
        charWidthSamples.isEmpty ? 8 : charWidthSamples.reduce(0, +) / CGFloat(charWidthSamples.count)
    }

    func start() {
        pollTimer = Timer.scheduledTimer(withTimeInterval: currentPollInterval, repeats: true) { [weak self] _ in
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
            DispatchQueue.main.async {
                OverlayWindow.shared.hide()
                PopupCardWindow.shared.hide()
                FieldEdgeIndicator.shared.hide()
            }
        }
        lastBundleId = bundleId
        lastPrefix = ""
    }

    func stop() {
        pollTimer?.invalidate()
        pollTimer = nil
        tearDownAXObserver()
        NSWorkspace.shared.notificationCenter.removeObserver(self)
    }

    /// Hide ONLY the field-edge "active" logo when focus is no longer on a text field,
    /// so it doesn't stay stuck on the old field after the user clicks another window or
    /// a non-text region of the same app. Crucially this must NOT hide the ghost-text
    /// overlay: `OverlayWindow.hide()` calls `KeyCapture.clearCompletion()`, so hiding it
    /// here would wipe the active completion and make Tab fall through. The overlay keeps
    /// its own hide paths (prefix change, app switch, accept/dismiss). UI-only — touches
    /// no poll bookkeeping (lastPrefix / lastBundleId).
    private func hideFieldIndicator() {
        DispatchQueue.main.async {
            FieldEdgeIndicator.shared.hide()
        }
    }

    /// Schedule a single screenshot→OCR capture for a newly focused field, after a
    /// short settle window (250 ms). A rapid field switch cancels the prior pending
    /// capture and re-arms, so a churning/flapping focus runs the pipeline once it
    /// stabilises rather than once per flap. The result is cached and reused for
    /// every completion in that field — no re-capture while the user keeps typing.
    private func scheduleFieldOCR(
        pid: pid_t, field: CGRect, bundle: String, fieldKey: String, fieldText: String
    ) {
        pendingOCRWork?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self else { return }
            // Bail if focus moved on during the settle window, or a capture is in flight.
            guard self.lastOCRFieldKey == fieldKey, !self.ocrInFlight else { return }
            self.ocrInFlight = true
            Task.detached(priority: .utility) { [weak self] in
                guard let self else { return }
                let text = await VisualContextCapture.shared.capture(
                    pid: pid, fieldFrameCG: field, fieldText: fieldText
                )
                await MainActor.run {
                    if let text = text {
                        self.latestVisualContext = text
                        self.latestVisualContextBundle = bundle
                    }
                    // Stamp even on failure so a field where capture can't succeed
                    // (no permission, no text) doesn't retry on every prefix change.
                    self.lastOCRCompletedAt = Date()
                    self.ocrInFlight = false
                }
            }
        }
        pendingOCRWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + AXMonitor.ocrSettleInterval, execute: work)
    }

    // ── AXObserver lifecycle ──────────────────────────────────────────────────

    private func tearDownAXObserver() {
        guard let obs = axObserver else { return }
        let source = AXObserverGetRunLoopSource(obs)
        CFRunLoopRemoveSource(CFRunLoopGetMain(), source, .defaultMode)
        axObserver = nil
        observedPid = 0
    }

    private func registerAXObserver(pid: pid_t) {
        tearDownAXObserver()
        let selfPtr = Unmanaged.passUnretained(self).toOpaque()
        // The callback must be @convention(c) — capture nothing, pass self via refcon.
        let callback: AXObserverCallback = { _, _, _, refcon in
            guard let refcon else { return }
            Unmanaged<AXMonitor>.fromOpaque(refcon).takeUnretainedValue().poll()
        }
        var obs: AXObserver?
        guard AXObserverCreate(pid, callback, &obs) == .success, let obs else { return }

        let appElement = AXUIElementCreateApplication(pid)

        // Electron gates its full accessibility tree behind this app-level attribute:
        // until an assistive client sets it (or VoiceOver is detected), apps like Slack
        // expose only a minimal tree where caret bounds queries fail and we're forced
        // into popup-card mode. Setting it is the documented Electron handshake; every
        // non-Electron app just returns .attributeUnsupported, so it's safe to send
        // unconditionally on each app switch.
        AXUIElementSetAttributeValue(
            appElement, "AXManualAccessibility" as CFString, kCFBooleanTrue
        )

        AXObserverAddNotification(obs, appElement, kAXValueChangedNotification as CFString, selfPtr)
        AXObserverAddNotification(obs, appElement, kAXSelectedTextChangedNotification as CFString, selfPtr)
        AXObserverAddNotification(obs, appElement, kAXFocusedUIElementChangedNotification as CFString, selfPtr)

        CFRunLoopAddSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(obs), .defaultMode)
        axObserver = obs
        observedPid = pid
    }

    // ── Adaptive backoff ──────────────────────────────────────────────────────

    private func reschedulePollTimer() {
        pollTimer?.invalidate()
        pollTimer = Timer.scheduledTimer(withTimeInterval: currentPollInterval, repeats: true) { [weak self] _ in
            self?.poll()
        }
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
        // No focused element in the (non-self) frontmost app: focus left any text
        // field — e.g. user clicked the desktop or a window with nothing editable
        // focused. Clear the stale indicator instead of leaving it pinned.
        guard result == .success, let element = focusedElement else { hideFieldIndicator(); return }
        let axElement = element as! AXUIElement

        // Register AXObserver when the app changes — ensures immediate notification
        // delivery for apps with reliable AX support; timer remains the backstop.
        if pid != observedPid { registerAXObserver(pid: pid) }

        // Check if it's a secure/password field
        var isSecure = false
        var secureValue: AnyObject?
        if AXUIElementCopyAttributeValue(axElement, "AXIsPasswordField" as CFString, &secureValue) == .success {
            isSecure = (secureValue as? Bool) ?? false
        }

        // Get the full text value. A focused element that exposes no string value is
        // not a text field (button, list row, web link, etc.) — the user moved focus
        // off the field within the same app, so hide the stale indicator.
        var textValue: AnyObject?
        guard AXUIElementCopyAttributeValue(axElement, kAXValueAttribute as CFString, &textValue) == .success,
              let fullText = textValue as? String
        else { hideFieldIndicator(); return }

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

        // Chromium/Electron fallback (Slack, Discord, web composers): these often fail
        // both index-based bounds queries above but speak WebKit's text-marker dialect —
        // the screen bounds of the (collapsed) selection marker range IS the caret rect.
        // Without this, Slack always lands in popup-card mode instead of inline ghost
        // text at the caret.
        if caretRect.height == 0, let webRect = AXMonitor.textMarkerCaretRect(for: axElement) {
            caretRect = webRect
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

        // Collect char-width samples for post-accept caret prediction.
        // Reset when font size or app changes to avoid stale averages.
        if axFontSize != lastFontSize || bundleId != lastBundleId {
            charWidthSamples.removeAll()
            lastFontSize = axFontSize
        }
        if caretRect.width > 1 {
            charWidthSamples.append(caretRect.width)
            if charWidthSamples.count > AXMonitor.maxCharWidthSamples {
                charWidthSamples.removeFirst()
            }
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
            DispatchQueue.main.async {
                OverlayWindow.shared.hide()
                PopupCardWindow.shared.hide()
            }
        }

        // Update the field-edge indicator on EVERY poll, not just on prefix changes:
        // scrolling, window moves, and clicking into another field don't change the
        // prefix, but they do move the field on screen — repositioning here keeps the
        // logo pinned to the field instead of stranded at its old location. (show()
        // skips the window churn when nothing moved.)
        let fieldFrame: CGRect? = inputFrameAX.height > 0
            ? CGRect(x: inputX, y: inputY, width: inputW, height: inputH) : nil
        let caretLineMidY: CGFloat? = caretRect.height > 0
            ? primaryScreenHeight - caretRect.origin.y - caretRect.height / 2 : nil
        DispatchQueue.main.async {
            if let f = fieldFrame { FieldEdgeIndicator.shared.show(inputFrame: f, caretLineMidY: caretLineMidY) }
            else { FieldEdgeIndicator.shared.hide() }
        }

        // Adaptive backoff: widen the poll interval while idle; snap back on change.
        if prefix == lastPrefix && bundleId == lastBundleId {
            unchangedPollCount += 1
            if unchangedPollCount >= 5 && currentPollInterval < 0.20 {
                currentPollInterval = min(currentPollInterval * 2, 0.20)
                unchangedPollCount = 0
                reschedulePollTimer()
            }
            return
        }
        if currentPollInterval > 0.08 {
            currentPollInterval = 0.08
            unchangedPollCount = 0
            reschedulePollTimer()
        } else {
            unchangedPollCount = 0
        }

        // Word-by-word partial accept: the user just accepted one word via Tab.
        // Instead of hiding the overlay and waiting for new inference (250ms+ gap),
        // reposition the remaining ghost text at the updated caret and skip the
        // contextUpdate so Rust doesn't re-infer. The flag is consumed exactly once.
        if bundleId == lastBundleId && KeyCapture.shared.isWordByWordInProgress {
            KeyCapture.shared.clearWordByWordFlag()
            lastPrefix = prefix
            let remaining = KeyCapture.shared.pendingCompletionText
            // Route through the same inline-vs-popup decision as a fresh completion:
            // in a popup-mode app (Slack etc.) the repositioned tail must go back to
            // the card, not be painted inline at an untrustworthy caret.
            if !remaining.isEmpty && (caretRect.height > 0 || inputFrameAX.height > 0) {
                let frame: CGRect? = inputFrameAX.height > 0
                    ? CGRect(x: inputX, y: inputY, width: inputW, height: inputH)
                    : nil
                let caretH = caretRect.height
                DispatchQueue.main.async {
                    OverlayRouter.present(
                        text: remaining, caretX: caretX, caretTopY: screenY,
                        caretHeight: caretH, fontSize: axFontSize,
                        inputFrame: frame, bundleId: bundleId)
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
        DispatchQueue.main.async {
            OverlayWindow.shared.hide()
            PopupCardWindow.shared.hide()
        }

        // ── Apple Intelligence engine bypass ──────────────────────────────────
        // When the user has selected the on-device Apple backend, complete locally
        // and bypass the Rust IPC entirely.  Only available on macOS 26+.
        #if canImport(FoundationModels)
        if #available(macOS 26, *) {
            if AppleIntelligenceBackend.isAvailable,
               UserDefaults.standard.string(forKey: "inferenceEngine") == "apple_intelligence" {
                let prefixSnap = prefix
                let appNameSnap = NSWorkspace.shared.runningApplications
                    .first(where: { $0.bundleIdentifier == bundleId })?
                    .localizedName ?? bundleId
                let caretXSnap = caretX, screenYSnap = screenY
                let caretHSnap = caretRect.height, fontSizeSnap = axFontSize
                let frameSnap: CGRect? = inputFrameAX.height > 0
                    ? CGRect(x: inputX, y: inputY, width: inputW, height: inputH) : nil

                aiDebounceWork?.cancel()
                let work = DispatchWorkItem { [weak self] in
                    guard let self else { return }
                    Task { @MainActor in
                        guard let text = await AppleIntelligenceBackend.complete(
                            prefix: prefixSnap, appName: appNameSnap
                        ), !text.isEmpty else { return }
                        OverlayRouter.present(
                            text: text, caretX: caretXSnap, caretTopY: screenYSnap,
                            caretHeight: caretHSnap, fontSize: fontSizeSnap,
                            inputFrame: frameSnap, bundleId: bundleId
                        )
                        KeyCapture.shared.setCompletion(text)
                        IPCBridge.shared.notify(method: "acceptCompletion", params: [:])
                    }
                    self.aiDebounceWork = nil
                }
                aiDebounceWork = work
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.02, execute: work)
                return
            }
        }
        #endif // canImport(FoundationModels)

        // Capture OCR once per focused field, then reuse it. Both
        // frames are AX coords (top-left origin), which is what ScreenCaptureKit's
        // sourceRect also uses — so no flips are needed downstream.
        //
        // Field identity = bundle + field frame + the AX element's identity. The frame
        // changes on a field switch; CFHash(element) disambiguates same-frame fields.
        // While you keep typing in one field this key is stable, so no re-capture fires.
        let fieldKey = "\(bundleId)|\(Int(inputX)),\(Int(inputY)),\(Int(inputW)),\(Int(inputH))|\(CFHash(axElement))"
        if fieldKey != lastOCRFieldKey {
            lastOCRFieldKey = fieldKey
            // Drop the previous field's excerpt so it can't leak into this field's
            // prompt; the new capture repopulates it once ready.
            latestVisualContext = ""
            latestVisualContextBundle = ""
            scheduleFieldOCR(pid: pid, field: inputFrameAX, bundle: bundleId,
                             fieldKey: fieldKey, fieldText: fullText)
        } else if !ocrInFlight,
                  Date().timeIntervalSince(lastOCRCompletedAt) > AXMonitor.ocrRefreshInterval {
            // Same field, still typing (we only reach here on a prefix change), but the
            // excerpt has aged: refresh so the prompt describes the CURRENT screen, not
            // the one from when the field was first focused. The old excerpt stays in
            // place until the new capture lands.
            scheduleFieldOCR(pid: pid, field: inputFrameAX, bundle: bundleId,
                             fieldKey: fieldKey, fieldText: fullText)
        }
        // Only reuse the cached OCR if it belongs to the app we're now typing in —
        // otherwise send nothing rather than another app's stale context.
        let visualCtxCopy = (latestVisualContextBundle == bundleId) ? latestVisualContext : ""

        // caretHeight=0 means AX couldn't determine caret bounds (Electron, terminal, etc.).
        // Cast CGFloat → Double explicitly: AnyCodable only encodes Double,
        // not CGFloat (a distinct Swift struct), so CGFloat values serialize as null.
        let appDisplayName = NSWorkspace.shared.runningApplications
            .first(where: { $0.bundleIdentifier == bundleId })?
            .localizedName ?? bundleId
        var clipboardContext = ""
        if UserDefaults.standard.bool(forKey: "clipboardContextEnabled") {
            let clip = NSPasteboard.general.string(forType: .string) ?? ""
            clipboardContext = clip.count > 200 ? String(clip.suffix(200)) : clip
        }
        IPCBridge.shared.notify(method: "contextUpdate", params: [
            "prefix":           prefix,
            "suffix":           suffix,
            "caretX":           Double(caretX),
            "caretY":           Double(screenY),
            "caretHeight":      Double(caretRect.height),  // 0 = no valid caret bounds
            "fontSize":         Double(axFontSize),         // 0 = unavailable
            "inputFrameX":      Double(inputX),
            "inputFrameY":      Double(inputY),
            "inputFrameW":      Double(inputW),
            "inputFrameH":      Double(inputH),
            "appBundleId":      bundleId,
            "appDisplayName":   appDisplayName,
            "isSecureField":    isSecure,
            "visualContext":    visualCtxCopy,
            "clipboardContext": clipboardContext,
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

    /// Caret rect via the WebKit text-marker AX dialect, which Chromium (and therefore
    /// Electron apps like Slack) implements where the index-based bounds-for-range
    /// queries fail or return zero rects. A collapsed selection's marker-range bounds
    /// is the caret. The attributes may live on the focused node or only on an
    /// ancestor (web area), so walk up a few parents. Returns nil when the dialect is
    /// unsupported or the rect is degenerate — callers keep their popup fallback.
    static func textMarkerCaretRect(for element: AXUIElement) -> CGRect? {
        var node = element
        for _ in 0..<4 {
            var markerRange: AnyObject?
            if AXUIElementCopyAttributeValue(
                node, "AXSelectedTextMarkerRange" as CFString, &markerRange
            ) == .success, let range = markerRange {
                var boundsVal: AnyObject?
                if AXUIElementCopyParameterizedAttributeValue(
                    node, "AXBoundsForTextMarkerRange" as CFString, range, &boundsVal
                ) == .success, let bv = boundsVal, CFGetTypeID(bv) == AXValueGetTypeID() {
                    var rect = CGRect.zero
                    if AXValueGetValue(bv as! AXValue, .cgRect, &rect),
                       rect.height > 0, rect.height < 200 {
                        return rect
                    }
                }
            }
            var parent: AnyObject?
            guard AXUIElementCopyAttributeValue(
                node, kAXParentAttribute as CFString, &parent
            ) == .success, let p = parent else { return nil }
            node = p as! AXUIElement
        }
        return nil
    }
}
