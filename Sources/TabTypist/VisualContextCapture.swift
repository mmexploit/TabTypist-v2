import AppKit
@preconcurrency import Vision
import ScreenCaptureKit

// Captures a compact screenshot of the focused window around the input field and
// extracts text via on-device Vision OCR:
//   • capture the focused window IN ISOLATION (desktopIndependentWindow) so other
//     windows can't occlude/clip the text,
//   • crop a field-centred band (field width + horizontal padding, a tall band above)
//     rather than the whole screen width,
//   • OCR with language correction OFF and a low minimumTextHeight so small chat text
//     is read in full instead of mangled ("Yeah" → "h").
//
// Screen Recording permission must be granted (optional — completions still work
// without it, just less context-aware).
final class VisualContextCapture: @unchecked Sendable {
    static let shared = VisualContextCapture()

    /// Character budget for OCR text injected into the prompt (keeps the tail, nearest the field).
    private static let maxChars = 1200
    /// Extra context captured left/right of the field, in display points.
    private static let horizontalPadding: CGFloat = 160
    /// Height of the band captured ABOVE the field, in display points.
    private static let verticalContextHeight: CGFloat = 800
    /// Downsample very large Retina captures before OCR to keep latency bounded. Vision's
    /// accurate mode benefits from pixels, but its cost scales with pixel area and a Retina
    /// capture of the band arrives well above this cap either way; 1200 keeps typical
    /// 11–13pt UI text comfortably above the recognition floor while cutting the Vision
    /// workload ~44% versus the previous 1600.
    private static let maxImageDimension = 1200

    /// Recent Vision extractions keyed by a pixel hash of the captured crop, so the
    /// age-gated refresh while typing skips the Vision pass — the dominant cost of this
    /// pipeline — when the screen hasn't actually changed (idle chat, static document).
    /// Only the raw geometry-filtered lines are cached: hygiene and capping rerun against
    /// the live field text below, so a hit stays identical to re-OCRing the same pixels.
    /// Single-flight access is guaranteed by AXMonitor's `ocrInFlight` gate.
    private var extractionCache: [(hash: UInt64, lines: [OCRHygiene.Line])] = []
    private static let extractionCacheLimit = 4

    private init() {}

    // MARK: – Public API

    /// `fieldFrameCG` is the focused field's bounds in CG global coords (top-left origin),
    /// i.e. the raw AX `AXFrame`. `pid` identifies the focused app so we can capture its
    /// window in isolation. `fieldText` is the field's current contents, used to strip
    /// OCR lines that merely re-read what the user already typed. Returns nil if
    /// permission is missing or no text is found.
    func capture(pid: pid_t, fieldFrameCG: CGRect, fieldText: String = "") async -> String? {
        guard CGPreflightScreenCaptureAccess() else {
            fputs("OCR: skipped — Screen Recording permission not granted\n", stderr)
            return nil
        }
        guard fieldFrameCG.width > 4 && fieldFrameCG.height > 4 else {
            fputs("OCR: skipped — invalid field frame \(fieldFrameCG)\n", stderr)
            return nil
        }
        guard let window = await focusedWindow(pid: pid) else {
            fputs("OCR: skipped — no on-screen window for pid \(pid)\n", stderr)
            return nil
        }

        let sourceRect = snapshotRect(fieldFrameCG: fieldFrameCG, windowFrame: window.frame)
        guard sourceRect.width > 20 && sourceRect.height > 20 else {
            fputs("OCR: skipped — crop too small \(sourceRect)\n", stderr)
            return nil
        }
        guard let image = await captureImage(window: window, sourceRect: sourceRect) else {
            fputs("OCR: skipped — ScreenCaptureKit returned no image\n", stderr)
            return nil
        }
        // The field's horizontal span inside the crop, normalized to [0,1] (Vision's
        // boundingBox space). Used to discard text from neighbouring columns.
        let spanLo = max(0, (fieldFrameCG.minX - sourceRect.minX) / sourceRect.width)
        let spanHi = min(1, (fieldFrameCG.maxX - sourceRect.minX) / sourceRect.width)
        let fieldSpan = spanLo...max(spanLo, spanHi)

        // Identical pixels OCR identically — reuse the cached extraction and skip the
        // Vision pass. (The geometry filter inside `recogniseLines` depends on the field
        // span, but identical pixels from the cache's small window imply the same crop
        // around the same field, so the cached lines were filtered with the same span.)
        let lines: [OCRHygiene.Line]
        let pixelHash = Self.pixelHash(of: image)
        if let pixelHash, let cached = cachedExtraction(for: pixelHash) {
            lines = cached
        } else {
            lines = await recogniseLines(in: image, fieldSpan: fieldSpan)
            storeExtraction(lines, for: pixelHash)
        }

        // Hygiene + capping rerun even on a cache hit: the field-echo strip compares
        // against the user's CURRENT text, which changes between captures.
        return excerpt(from: lines, fieldText: fieldText)
    }

    // MARK: – Window discovery

    private func focusedWindow(pid: pid_t) async -> SCWindow? {
        guard let content = try? await SCShareableContent.excludingDesktopWindows(
            true, onScreenWindowsOnly: true
        ) else { return nil }
        return content.windows.first(where: {
            $0.owningApplication?.processID == pid && $0.isActive && $0.isOnScreen
        }) ?? content.windows.first(where: {
            $0.owningApplication?.processID == pid && $0.isOnScreen
        })
    }

    // MARK: – Crop geometry (all CG/top-left coords)

    private func snapshotRect(fieldFrameCG: CGRect, windowFrame: CGRect) -> CGRect {
        let targetHeight = min(Self.verticalContextHeight, windowFrame.height)
        let targetWidth = min(fieldFrameCG.width + Self.horizontalPadding * 2, windowFrame.width)
        let proposedX = fieldFrameCG.minX - Self.horizontalPadding
        let proposedY = fieldFrameCG.minY - targetHeight   // band ABOVE the field (smaller y)
        // Clamp inside the window so ScreenCaptureKit doesn't fail or crop incorrectly.
        let clampedX = min(max(proposedX, windowFrame.minX), windowFrame.maxX - targetWidth)
        let clampedY = min(max(proposedY, windowFrame.minY), windowFrame.maxY - targetHeight)
        return CGRect(x: clampedX, y: clampedY, width: targetWidth, height: targetHeight).integral
    }

    // MARK: – Screen capture (ScreenCaptureKit)

    private func captureImage(window: SCWindow, sourceRect: CGRect) async -> CGImage? {
        let scale = backingScaleFactor(forCG: sourceRect)
        // sourceRect is global; desktopIndependentWindow wants window-local coords.
        let local = CGRect(
            x: sourceRect.minX - window.frame.minX,
            y: sourceRect.minY - window.frame.minY,
            width: sourceRect.width,
            height: sourceRect.height
        )
        let filter = SCContentFilter(desktopIndependentWindow: window)
        let config = SCStreamConfiguration()
        config.sourceRect = local
        config.width  = max(Int((local.width  * scale).rounded(.up)), 1)
        config.height = max(Int((local.height * scale).rounded(.up)), 1)
        config.showsCursor = false
        return try? await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
    }

    /// Backing scale of the screen containing the crop's midpoint. `rect` is CG/top-left;
    /// convert its midpoint to AppKit (bottom-left) to test against NSScreen frames.
    private func backingScaleFactor(forCG rect: CGRect) -> CGFloat {
        let desktop = NSScreen.screens.map(\.frame).reduce(CGRect.null) { $0.union($1) }
        let appKitMid = CGPoint(x: rect.midX, y: desktop.maxY - rect.midY)
        let screen = NSScreen.screens.first(where: { $0.frame.contains(appKitMid) }) ?? NSScreen.main
        return screen?.backingScaleFactor ?? 2.0
    }

    // MARK: – OCR

    /// Vision pass plus geometric filtering — everything that depends only on the pixels,
    /// so the result is cacheable by pixel hash. Hygiene (which depends on the live field
    /// text) happens in `excerpt(from:fieldText:)`.
    private func recogniseLines(
        in image: CGImage, fieldSpan: ClosedRange<CGFloat>
    ) async -> [OCRHygiene.Line] {
        let prepared = downsampled(image)
        return await withCheckedContinuation { continuation in
            let request = VNRecognizeTextRequest { request, _ in
                let observations = request.results as? [VNRecognizedTextObservation] ?? []
                // Column filter: keep only text whose box overlaps the field's own
                // column. The crop's horizontal padding can reach into a neighbouring
                // pane (Slack's channel sidebar), and the band sort below would then
                // splice sidebar rows INTO message sentences ("Can you try | Elizabeth
                // Wheeler | again?"). The field column is where the content the user is
                // replying to lives; everything beside it is navigation chrome.
                let lo = fieldSpan.lowerBound - 0.02, hi = fieldSpan.upperBound + 0.02
                let lines = observations
                    .filter { obs in
                        let box = obs.boundingBox
                        // Drop lines touching the crop's bottom edge: they sit half
                        // behind the input field / crop boundary and OCR garbles the
                        // clipped glyphs into nonsense words.
                        return box.maxX >= lo && box.minX <= hi && box.minY > 0.012
                    }
                    // Reading order: top-to-bottom (bands), then left-to-right within a band.
                    .sorted {
                        if abs($0.boundingBox.minY - $1.boundingBox.minY) > 0.02 {
                            return $0.boundingBox.minY > $1.boundingBox.minY
                        }
                        return $0.boundingBox.minX < $1.boundingBox.minX
                    }
                    .compactMap { obs -> OCRHygiene.Line? in
                        guard let top = obs.topCandidates(1).first else { return nil }
                        let trimmed = top.string.trimmingCharacters(in: .whitespacesAndNewlines)
                        guard !trimmed.isEmpty else { return nil }
                        return OCRHygiene.Line(text: trimmed, confidence: top.confidence)
                    }
                continuation.resume(returning: lines)
            }
            // Accurate, with language correction: this text only conditions the prompt
            // (it is never shown or inserted), and correction cuts garbled recognitions
            // at the source — the hygiene filters downstream can only drop junk, not
            // repair it. A low minimum text height keeps small chat text readable.
            request.recognitionLevel = .accurate
            request.usesLanguageCorrection = true
            request.minimumTextHeight = 0.008

            do {
                try VNImageRequestHandler(cgImage: prepared, options: [:]).perform([request])
            } catch {
                continuation.resume(returning: [])
            }
        }
    }

    /// Structural hygiene + prompt budgeting over the recognised lines. Reruns on every
    /// capture — including pixel-cache hits — because the field-echo strip compares
    /// against the user's current text.
    private func excerpt(from raw: [OCRHygiene.Line], fieldText: String) -> String? {
        // Structural hygiene (general, app-agnostic): drops low-confidence and
        // corrupted lines, symbol/glyph noise, digit-substituted misreads,
        // timestamp/view-count chrome, and echoes of the user's own field text.
        let lines = OCRHygiene.clean(raw, fieldText: fieldText)
        let joined = lines.joined(separator: "\n")
        // Keep the tail (nearest the field = most recent in a chat). Cut at a
        // line boundary so the excerpt never starts mid-sentence.
        var capped = joined.count <= Self.maxChars
            ? joined
            : String(joined.suffix(Self.maxChars))
        if capped.count < joined.count, let nl = capped.firstIndex(of: "\n") {
            capped = String(capped[capped.index(after: nl)...])
        }
        let flat = capped.replacingOccurrences(of: "\n", with: " | ")
        fputs("OCR: \(lines.count) lines, \(capped.count) chars: \(flat)\n", stderr)
        return capped.isEmpty ? nil : capped
    }

    // MARK: – Pixel-hash extraction cache

    /// FNV-1a over a strided sample of the image bytes, mixed with the dimensions.
    /// Sampling keeps the hash sub-millisecond on Retina crops while touching every row;
    /// any real content change moves enough antialiased pixels that a stride collision is
    /// vanishingly unlikely, and the worst case of one is reusing OCR text for a window
    /// whose pixels barely changed. The stride is 17, not 16: with 4-byte pixels a
    /// multiple-of-4 stride lands on the same colour channel forever, so a chroma-only
    /// change (theme toggle with unchanged luminance) could hash identically; a stride
    /// coprime with the pixel size cycles through all four channels. `nil` (no readable
    /// backing data) simply disables caching.
    private static func pixelHash(of image: CGImage) -> UInt64? {
        guard let data = image.dataProvider?.data,
              let bytes = CFDataGetBytePtr(data) else {
            return nil
        }
        let length = CFDataGetLength(data)
        let prime: UInt64 = 0x0000_0100_0000_01B3
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        var index = 0
        while index < length {
            hash = (hash ^ UInt64(bytes[index])) &* prime
            index += 17
        }
        hash = (hash ^ UInt64(image.width)) &* prime
        hash = (hash ^ UInt64(image.height)) &* prime
        return hash
    }

    private func cachedExtraction(for hash: UInt64) -> [OCRHygiene.Line]? {
        extractionCache.first(where: { $0.hash == hash })?.lines
    }

    private func storeExtraction(_ lines: [OCRHygiene.Line], for hash: UInt64?) {
        guard let hash else { return }
        extractionCache.removeAll { $0.hash == hash }
        extractionCache.append((hash, lines))
        if extractionCache.count > Self.extractionCacheLimit {
            extractionCache.removeFirst(extractionCache.count - Self.extractionCacheLimit)
        }
    }

    /// Scale very large Retina captures down to a bounded dimension before OCR.
    private func downsampled(_ image: CGImage) -> CGImage {
        let largest = max(image.width, image.height)
        guard largest > Self.maxImageDimension else { return image }
        let scale = CGFloat(Self.maxImageDimension) / CGFloat(largest)
        let w = max(Int(CGFloat(image.width) * scale), 1)
        let h = max(Int(CGFloat(image.height) * scale), 1)
        let colorSpace = image.colorSpace ?? CGColorSpaceCreateDeviceRGB()
        guard let ctx = CGContext(
            data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
            space: colorSpace, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else { return image }
        ctx.interpolationQuality = .medium
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
        return ctx.makeImage() ?? image
    }
}
