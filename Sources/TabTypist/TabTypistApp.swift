import AppKit
import ApplicationServices
import Sparkle
import SwiftUI

// TabTypist is a macOS menu bar app.
// It spawns the Rust core (tabtypist-core) as a subprocess and communicates
// with it via newline-delimited JSON-RPC on stdin/stdout.
//
// In the app bundle: TabTypist (Swift, this binary) is the main process.
//   → spawns tabtypist-core (Rust) as a child, pipes stdin/stdout bidirectionally.
// In standalone dev mode: the Rust binary can spawn this binary directly.
//   → IPCBridge falls back to reading from our stdin / writing to our stdout.
// See docs/adr/0005-swift-launches-rust-in-app-bundle.md.

@main
struct TabTypistApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var delegate

    var body: some Scene {
        Settings { EmptyView() }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate, @unchecked Sendable {
    private var coreProcess: Process?
    private var updaterController: SPUStandardUpdaterController?
    private var checkForUpdatesCancellable: Any?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)

        updaterController = SPUStandardUpdaterController(
            startingUpdater: true,
            updaterDelegate: nil,
            userDriverDelegate: nil
        )

        checkForUpdatesCancellable = NotificationCenter.default.addObserver(
            forName: .checkForUpdatesRequested,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.updaterController?.checkForUpdates(nil)
        }

        // Set up menu bar
        MenuBarController.shared.setup()

        // Wire message handler before starting bridge
        IPCBridge.shared.onMessage = { [weak self] msg in
            self?.handleCoreMessage(msg)
        }

        // Spawn tabtypist-core as a subprocess (configures IPCBridge in app mode)
        spawnCore()

        // Start AX monitor and key capture
        AXMonitor.shared.start()
        KeyCapture.shared.start()

        // If Accessibility isn't granted, open onboarding immediately.
        if !AXIsProcessTrusted() {
            OnboardingController.shared.showIfNeeded()
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        AXMonitor.shared.stop()
        coreProcess?.terminate()
    }

    // ── Core subprocess ───────────────────────────────────────────────────────

    private func spawnCore() {
        let corePath = coreBinaryPath()

        guard !corePath.isEmpty, FileManager.default.fileExists(atPath: corePath) else {
            fputs("TabTypist: tabtypist-core not found at '\(corePath)'\n", stderr)
            fputs("TabTypist: set TABTYPIST_CORE_PATH or place the binary in Resources.\n", stderr)
            // Fall back to standalone mode (IPCBridge reads from stdin)
            IPCBridge.shared.start()
            return
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: corePath)
        process.environment = ProcessInfo.processInfo.environment

        let stdinPipe = Pipe()
        let stdoutPipe = Pipe()
        process.standardInput = stdinPipe
        process.standardOutput = stdoutPipe
        process.standardError = FileHandle.standardError

        do {
            try process.run()
        } catch {
            fputs("TabTypist: failed to launch core: \(error)\n", stderr)
            IPCBridge.shared.start()
            return
        }

        coreProcess = process

        // Configure bridge in app mode
        IPCBridge.shared.configure(
            readHandle: stdoutPipe.fileHandleForReading,
            writeHandle: stdinPipe.fileHandleForWriting
        )
        IPCBridge.shared.start()
    }

    private func coreBinaryPath() -> String {
        // App bundle: Contents/Resources/tabtypist-core
        if let resourcePath = Bundle.main.resourcePath {
            let bundled = resourcePath + "/tabtypist-core"
            if FileManager.default.fileExists(atPath: bundled) { return bundled }
        }
        // Development: next to this binary
        if let exe = Bundle.main.executablePath {
            let dir = (exe as NSString).deletingLastPathComponent
            let dev = dir + "/tabtypist-core"
            if FileManager.default.fileExists(atPath: dev) { return dev }
        }
        // Env override
        return ProcessInfo.processInfo.environment["TABTYPIST_CORE_PATH"] ?? ""
    }

    // ── Message handler ───────────────────────────────────────────────────────

    private func handleCoreMessage(_ msg: RpcMessage) {
        let method = msg.method ?? ""

        // Handle ping in standalone mode (Rust is parent)
        if method == "ping", let id = msg.id {
            IPCBridge.shared.respond(id: id, result: "pong")
            return
        }

        let params = (msg.params?.value as? [String: Any]) ?? [:]

        DispatchQueue.main.async {
            // serde_json may serialize whole-number f64 values as JSON integers,
            // causing AnyCodable to store them as Int rather than Double.
            func cgf(_ v: Any?, fallback: CGFloat = 0) -> CGFloat {
                if let d = v as? Double { return CGFloat(d) }
                if let i = v as? Int    { return CGFloat(i) }
                return fallback
            }

            switch method {
            case "showOverlay":
                let x        = cgf(params["x"])
                let y        = cgf(params["y"])
                let height   = cgf(params["height"], fallback: 16)
                let fontSize = cgf(params["fontSize"])
                let fw       = cgf(params["inputFrameW"])
                let fh       = cgf(params["inputFrameH"])
                let inputFrame: CGRect? = (fw > 0 && fh > 0)
                    ? CGRect(x: cgf(params["inputFrameX"]), y: cgf(params["inputFrameY"]),
                             width: fw, height: fh)
                    : nil
                let text     = (params["text"] as? String) ?? ""
                let bundleId = (params["appBundleId"] as? String) ?? ""
                fputs("TabTypist showOverlay received: x=\(x) y=\(y) h=\(height) fs=\(fontSize) text=\(text.prefix(40))\n", stderr)

                OverlayRouter.present(
                    text: text, caretX: x, caretTopY: y, caretHeight: height,
                    fontSize: fontSize, inputFrame: inputFrame, bundleId: bundleId
                )
                KeyCapture.shared.setCompletion(text)

            case "acceptCompletion":
                OverlayWindow.recordAcceptance()

            case "hideOverlay":
                OverlayWindow.shared.hide()
                PopupCardWindow.shared.hide()
                KeyCapture.shared.clearCompletion()

            case "showMessagingToast":
                let bundleId = (params["bundleId"] as? String) ?? ""
                let appName = NSWorkspace.shared.runningApplications
                    .first(where: { $0.bundleIdentifier == bundleId })?
                    .localizedName ?? bundleId
                ToastManager.shared.showMessagingToast(bundleId: bundleId, appName: appName)
                MenuBarController.shared.update(appName: appName, active: true)

            case "modelLoaded":
                let tier        = (params["tier"]        as? String) ?? ""
                let displayName = (params["displayName"] as? String) ?? ""
                UserDefaults.standard.set(tier, forKey: "activeModelTier")
                MenuBarController.shared.modelLoaded(tier: tier, displayName: displayName)

            case "updateMenuBar":
                let appName = (params["appName"] as? String) ?? ""
                let active  = (params["active"]  as? Bool)   ?? true
                MenuBarController.shared.update(appName: appName, active: active)

            case "ready":
                let needsOnboarding = (params["needsOnboarding"] as? Bool) ?? false
                if needsOnboarding {
                    OnboardingController.shared.showIfNeeded()
                }

            case "downloadProgress":
                // Forward all params directly so ModelDownloadStep can pick up
                // phase, downloaded, total, progress, and error fields.
                var userInfo: [String: Any] = [:]
                if let phase    = params["phase"]      as? String { userInfo["phase"]      = phase }
                if let dl       = params["downloaded"]  as? Int    { userInfo["downloaded"]  = Int64(dl) }
                if let tot      = params["total"]       as? Int    { userInfo["total"]       = Int64(tot) }
                if let prog     = params["progress"]    as? Double { userInfo["progress"]    = prog }
                if let err      = params["error"]       as? String { userInfo["error"]       = err }
                NotificationCenter.default.post(
                    name: .downloadProgressUpdated, object: nil, userInfo: userInfo
                )

            default:
                break
            }
        }
    }
}

extension Notification.Name {
    static let downloadProgressUpdated = Notification.Name("TabTypist.downloadProgressUpdated")
    static let checkForUpdatesRequested = Notification.Name("TabTypist.checkForUpdatesRequested")
}
