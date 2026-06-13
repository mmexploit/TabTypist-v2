import AppKit
import ApplicationServices
import Combine
import SwiftUI
import Darwin  // for sysctlbyname

// ── Shared state across onboarding steps ─────────────────────────────────────

// Mirrors the Rust ModelCatalog so the tier picker can display without IPC.
struct ModelTierInfo: Identifiable {
    let id: String
    let tier: String
    let displayName: String
    let sizeGB: Double
    let minRAMGB: Int
    let isInstruct: Bool

    // Base checkpoints only — instruct models reply to context and leak chat
    // scaffolding into ghost text, so every tier now uses the base-continuation path.
    // displayName is the user-facing branded name; the raw model family stays in `id`.
    static let catalog: [ModelTierInfo] = [
        ModelTierInfo(id: "qwen3-0.6b-base-q4km",  tier: "nano",        displayName: "Nano",        sizeGB: 0.40, minRAMGB: 0,  isInstruct: false),
        ModelTierInfo(id: "qwen35-0.8b-base-q6k",  tier: "mini",        displayName: "Mini",        sizeGB: 0.63, minRAMGB: 8,  isInstruct: false),
        ModelTierInfo(id: "qwen35-2b-base-q4km",   tier: "standard",    displayName: "Standard",    sizeGB: 1.27, minRAMGB: 8,  isInstruct: false),
        ModelTierInfo(id: "qwen3-4b-base-q4km",    tier: "performance", displayName: "Performance", sizeGB: 2.50, minRAMGB: 16, isInstruct: false),
        ModelTierInfo(id: "gemma4-e2b-base-q6k",   tier: "quality",     displayName: "Quality",     sizeGB: 3.85, minRAMGB: 16, isInstruct: false),
        ModelTierInfo(id: "gemma4-e4b-base-q4km",  tier: "pro",         displayName: "Pro",         sizeGB: 5.34, minRAMGB: 24, isInstruct: false),
    ]

    static func brandedName(for tier: String) -> String {
        catalog.first(where: { $0.tier == tier })?.displayName ?? tier.capitalized
    }

    var sizeLabel: String { String(format: "%.1f GB", sizeGB) }
    var ramLabel: String  { minRAMGB == 0 ? "Any Mac" : "\(minRAMGB) GB+ RAM" }
}

func detectPhysicalRAMGB() -> Int {
    var size: UInt64 = 0
    var sizeLen = MemoryLayout<UInt64>.size
    sysctlbyname("hw.memsize", &size, &sizeLen, nil, 0)
    return Int(size / (1024 * 1024 * 1024))
}

func recommendedTier(ramGB: Int) -> String {
    if ramGB >= 24 { return "pro" }
    if ramGB >= 16 { return "quality" }
    if ramGB >= 8  { return "standard" }
    return "nano"
}

final class OnboardingState: ObservableObject {
    @Published var selectedLanguages: Set<String> = ["en"]
    @Published var selectedTierId: String = {
        let ram = detectPhysicalRAMGB()
        let tier = recommendedTier(ramGB: ram)
        return ModelTierInfo.catalog.first(where: { $0.tier == tier })?.id
            ?? "qwen35-2b-base-q4km"
    }()

    // Download state
    @Published var downloadedBytes: Int64 = 0
    @Published var totalBytes: Int64 = 0
    @Published var downloadPhase: DownloadPhase = .idle

    enum DownloadPhase: Equatable {
        case idle
        case downloading
        case verifying
        case complete
        case failed(String)
    }

    var downloadFraction: Double {
        guard totalBytes > 0 else { return 0 }
        return min(1.0, Double(downloadedBytes) / Double(totalBytes))
    }

    func formattedBytes(_ n: Int64) -> String {
        let mb = Double(n) / 1_000_000
        return mb >= 1000 ? String(format: "%.2f GB", mb / 1000) : String(format: "%.0f MB", mb)
    }

    var downloadLabel: String {
        switch downloadPhase {
        case .idle:    return "Ready to download"
        case .downloading:
            if totalBytes > 0 {
                return "\(formattedBytes(downloadedBytes)) of \(formattedBytes(totalBytes))"
            }
            return "Downloading…"
        case .verifying: return "Verifying…"
        case .complete:  return "Download complete"
        case .failed(let e): return "Failed: \(e)"
        }
    }
}

// ── Controller ────────────────────────────────────────────────────────────────

final class OnboardingController {
    static let shared = OnboardingController()

    private var window: NSWindow?
    private var hostingView: NSHostingView<OnboardingView>?

    func showIfNeeded() {
        DispatchQueue.main.async { self.show() }
    }

    func show() {
        if let w = window, w.isVisible { w.makeKeyAndOrderFront(nil); return }

        let state = OnboardingState()
        let view = OnboardingView(state: state)
        let hosting = NSHostingView(rootView: view)
        hosting.frame = NSRect(x: 0, y: 0, width: 580, height: 460)

        let w = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 580, height: 460),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false
        )
        w.title = "Welcome to TabTypist"
        w.contentView = hosting
        w.center()
        w.makeKeyAndOrderFront(nil)
        w.isReleasedWhenClosed = false
        window = w
        hostingView = hosting
    }

    func dismiss() {
        window?.close()
        window = nil
        hostingView = nil
    }
}

// ── Model picker (standalone — skips welcome / permissions / intro) ───────────

final class ModelPickerController {
    static let shared = ModelPickerController()

    private var window: NSWindow?
    private var hostingView: NSHostingView<ModelPickerView>?

    func show() {
        if let w = window, w.isVisible { w.makeKeyAndOrderFront(nil); return }

        let state = OnboardingState()
        let view = ModelPickerView(state: state) { [weak self] in self?.dismiss() }
        let hosting = NSHostingView(rootView: view)
        hosting.frame = NSRect(x: 0, y: 0, width: 560, height: 440)

        let w = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 560, height: 440),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false
        )
        w.title = "Change Model"
        w.contentView = hosting
        w.center()
        w.makeKeyAndOrderFront(nil)
        w.isReleasedWhenClosed = false
        window = w
        hostingView = hosting
    }

    func dismiss() {
        window?.close()
        window = nil
        hostingView = nil
    }
}

// ── Phases ────────────────────────────────────────────────────────────────────

enum OnboardingPhase: Int, CaseIterable {
    case welcome = 1
    case languageSelect
    case accessibilityPermission
    case screenRecordingPermission
    case modelDownload
    case done
}

// ── Container view ────────────────────────────────────────────────────────────

struct OnboardingView: View {
    @ObservedObject var state: OnboardingState
    @State private var phase: OnboardingPhase = .welcome
    @State private var accessibilityGranted: Bool = false
    @State private var screenRecordingGranted: Bool = false

    var body: some View {
        VStack(spacing: 0) {
            // Progress dots
            HStack(spacing: 6) {
                ForEach(1...5, id: \.self) { i in
                    Circle()
                        .fill(i <= phase.rawValue ? Color.accentColor : Color.secondary.opacity(0.3))
                        .frame(width: 6, height: 6)
                }
            }
            .padding(.top, 20)

            phaseView
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .transition(.asymmetric(
                    insertion: .move(edge: .trailing).combined(with: .opacity),
                    removal: .move(edge: .leading).combined(with: .opacity)
                ))
                .id(phase)

            Divider()

            HStack {
                if phase != .welcome && phase != .done {
                    Button("Back") { withAnimation { retreat() } }
                        .disabled(phase == .modelDownload && state.downloadPhase == .downloading)
                }
                Spacer()
                nextButton
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 16)
        }
        .frame(width: 580, height: 460)
        .onChange(of: state.downloadPhase) { _, newPhase in
            if case .complete = newPhase {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.8) {
                    withAnimation { advance() }
                }
            }
        }
    }

    @ViewBuilder
    private var phaseView: some View {
        switch phase {
        case .welcome:
            WelcomeStep()
        case .languageSelect:
            LanguageSelectStep(state: state)
        case .accessibilityPermission:
            AccessibilityStep(granted: $accessibilityGranted)
        case .screenRecordingPermission:
            ScreenRecordingStep(granted: $screenRecordingGranted)
        case .modelDownload:
            ModelDownloadStep(state: state)
        case .done:
            DoneStep()
        }
    }

    private var nextButton: some View {
        Group {
            switch phase {
            case .welcome:
                Button("Get Started") { withAnimation { advance() } }
                    .buttonStyle(.borderedProminent)

            case .languageSelect:
                Button("Continue") { withAnimation { advance() } }
                    .buttonStyle(.borderedProminent)
                    .disabled(state.selectedLanguages.isEmpty)

            case .accessibilityPermission:
                if accessibilityGranted {
                    Button("Continue") { withAnimation { advance() } }
                        .buttonStyle(.borderedProminent)
                } else {
                    Button("Grant Accessibility…") { requestAccessibility() }
                        .buttonStyle(.borderedProminent)
                }

            case .screenRecordingPermission:
                if screenRecordingGranted {
                    Button("Continue") { withAnimation { advance() } }
                        .buttonStyle(.borderedProminent)
                } else {
                    HStack(spacing: 12) {
                        // Optional: completions still work without screen context.
                        Button("Skip") { withAnimation { advance() } }
                        Button("Grant Screen Recording…") { requestScreenRecording() }
                            .buttonStyle(.borderedProminent)
                    }
                }

            case .modelDownload:
                switch state.downloadPhase {
                case .idle:
                    Button("Download Model") { startDownload() }
                        .buttonStyle(.borderedProminent)
                case .downloading, .verifying:
                    Button("Downloading…") {}
                        .buttonStyle(.borderedProminent)
                        .disabled(true)
                case .complete:
                    Button("Continue") { withAnimation { advance() } }
                        .buttonStyle(.borderedProminent)
                case .failed:
                    Button("Retry") { startDownload() }
                        .buttonStyle(.borderedProminent)
                }

            case .done:
                Button("Start Typing") { finish() }
                    .buttonStyle(.borderedProminent)
            }
        }
    }

    private func advance() {
        let next = phase.rawValue + 1
        phase = OnboardingPhase(rawValue: next) ?? .done
    }

    private func retreat() {
        let prev = phase.rawValue - 1
        phase = OnboardingPhase(rawValue: prev) ?? .welcome
    }

    private func requestAccessibility() {
        let opts = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true]
        accessibilityGranted = AXIsProcessTrustedWithOptions(opts as CFDictionary)
        if accessibilityGranted { withAnimation { advance() } }
    }

    private func requestScreenRecording() {
        // Surfaces the system prompt and adds TabTypist to the Screen Recording list.
        // The grant typically only takes effect after a relaunch, so we don't auto-
        // advance here; the step polls CGPreflightScreenCaptureAccess() and flips to
        // the granted state if macOS reports it live.
        _ = CGRequestScreenCaptureAccess()
        screenRecordingGranted = CGPreflightScreenCaptureAccess()
    }

    private func startDownload() {
        let modelId = state.selectedTierId
        state.downloadPhase = .downloading
        IPCBridge.shared.notify(method: "startModelDownload", params: [
            "language": "en",
            "modelId": modelId,
        ])
    }

    private func finish() {
        IPCBridge.shared.notify(method: "onboardingComplete", params: [:])
        OnboardingController.shared.dismiss()
    }
}

// ── Step views ────────────────────────────────────────────────────────────────

struct WelcomeStep: View {
    var body: some View {
        VStack(spacing: 24) {
            Image(systemName: "text.cursor")
                .font(.system(size: 64))
                .symbolRenderingMode(.hierarchical)
                .foregroundStyle(.blue)

            VStack(spacing: 10) {
                Text("Welcome to TabTypist")
                    .font(.largeTitle.bold())
                Text("Ghost-text completions as you type — in every app on your Mac.\nRuns entirely on your device. No cloud, no subscriptions.")
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: 400)
            }

            HStack(spacing: 28) {
                FeaturePill(icon: "lock.shield.fill",  text: "100% local",    color: .green)
                FeaturePill(icon: "bolt.fill",         text: "Tab to accept", color: .blue)
                FeaturePill(icon: "xmark.circle.fill", text: "Esc to dismiss", color: .orange)
            }
        }
        .padding(.horizontal, 40)
        .padding(.vertical, 32)
    }
}

struct FeaturePill: View {
    let icon: String
    let text: String
    let color: Color
    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: icon).foregroundStyle(color)
            Text(text).font(.callout)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(color.opacity(0.1), in: Capsule())
    }
}

struct LanguageSelectStep: View {
    @ObservedObject var state: OnboardingState

    var body: some View {
        VStack(spacing: 24) {
            Text("Choose Your Languages")
                .font(.title2.bold())
            Text("TabTypist downloads a model for each language. You can change this later.")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
                .frame(maxWidth: 400)

            VStack(spacing: 12) {
                LanguageRow(
                    name: "English",
                    modelInfo: "Standard tier · ~1.3 GB",
                    flag: "🇬🇧",
                    isSelected: state.selectedLanguages.contains("en")
                ) {
                    toggleLanguage("en")
                }
                .disabled(true) // English is always required at v1
            }
            .frame(maxWidth: 400)

            Text("More languages coming soon")
                .font(.caption)
                .foregroundStyle(.tertiary)
        }
        .padding(.horizontal, 40)
        .padding(.vertical, 32)
    }

    private func toggleLanguage(_ code: String) {
        if state.selectedLanguages.contains(code) {
            state.selectedLanguages.remove(code)
        } else {
            state.selectedLanguages.insert(code)
        }
        let langs = Array(state.selectedLanguages)
        IPCBridge.shared.notify(method: "updateSetting", params: ["key": "languages", "value": langs])
    }
}

struct LanguageRow: View {
    let name: String
    let modelInfo: String
    let flag: String
    let isSelected: Bool
    let onToggle: () -> Void

    var body: some View {
        Button(action: onToggle) {
            HStack(spacing: 14) {
                Text(flag).font(.title2)
                VStack(alignment: .leading, spacing: 2) {
                    Text(name).font(.body.weight(.medium))
                    Text(modelInfo).font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Image(systemName: isSelected ? "checkmark.circle.fill" : "circle")
                    .font(.title3)
                    .foregroundStyle(isSelected ? Color.accentColor : Color.secondary)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .background(
                RoundedRectangle(cornerRadius: 10)
                    .fill(isSelected ? Color.accentColor.opacity(0.08) : Color.secondary.opacity(0.06))
                    .overlay(
                        RoundedRectangle(cornerRadius: 10)
                            .stroke(isSelected ? Color.accentColor.opacity(0.4) : .clear, lineWidth: 1.5)
                    )
            )
        }
        .buttonStyle(.plain)
    }
}

struct AccessibilityStep: View {
    @Binding var granted: Bool
    @State private var isPolling = false

    var body: some View {
        VStack(spacing: 24) {
            Image(systemName: granted ? "checkmark.circle.fill" : "hand.raised.fill")
                .font(.system(size: 56))
                .foregroundStyle(granted ? .green : .orange)
                .animation(.spring, value: granted)

            VStack(spacing: 10) {
                Text("Accessibility Access")
                    .font(.title2.bold())
                Text("TabTypist uses macOS Accessibility to read caret position and insert text when you press Tab. This stays on your device — nothing is sent anywhere.")
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: 400)
            }

            if granted {
                Label("Access granted", systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                    .font(.callout.weight(.medium))
            } else {
                VStack(spacing: 8) {
                    Text("After clicking the button below:")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text("System Settings → Privacy & Security → Accessibility → enable TabTypist")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 380)
                }
                .padding(12)
                .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
            }
        }
        .padding(.horizontal, 40)
        .padding(.vertical, 32)
        .onAppear {
            granted = AXIsProcessTrusted()
            startPolling()
        }
        .onDisappear { isPolling = false }
    }

    private func startPolling() {
        isPolling = true
        Task {
            while isPolling && !granted {
                try? await Task.sleep(nanoseconds: 500_000_000) // 0.5s
                await MainActor.run { granted = AXIsProcessTrusted() }
            }
        }
    }
}

struct ScreenRecordingStep: View {
    @Binding var granted: Bool
    @State private var isPolling = false

    var body: some View {
        VStack(spacing: 24) {
            Image(systemName: granted ? "checkmark.circle.fill" : "rectangle.dashed.badge.record")
                .font(.system(size: 56))
                .foregroundStyle(granted ? .green : .orange)
                .animation(.spring, value: granted)

            VStack(spacing: 10) {
                Text("Screen Recording Access")
                    .font(.title2.bold())
                Text("Optional. TabTypist reads on-screen text near the field (e.g. the conversation you're replying to) so suggestions fit the context. The capture is processed on-device with OCR and never leaves your Mac.")
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: 400)
            }

            if granted {
                Label("Access granted", systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                    .font(.callout.weight(.medium))
            } else {
                VStack(spacing: 8) {
                    Text("After clicking the button below:")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text("System Settings → Privacy & Security → Screen Recording → enable TabTypist")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 380)
                    Text("macOS may ask you to quit & reopen TabTypist for this to take effect.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 380)
                }
                .padding(12)
                .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
            }
        }
        .padding(.horizontal, 40)
        .padding(.vertical, 32)
        .onAppear {
            granted = CGPreflightScreenCaptureAccess()
            startPolling()
        }
        .onDisappear { isPolling = false }
    }

    private func startPolling() {
        isPolling = true
        Task {
            while isPolling && !granted {
                try? await Task.sleep(nanoseconds: 500_000_000) // 0.5s
                await MainActor.run { granted = CGPreflightScreenCaptureAccess() }
            }
        }
    }
}

struct ModelDownloadStep: View {
    @ObservedObject var state: OnboardingState

    private let ramGB = detectPhysicalRAMGB()
    private var recommendedId: String {
        let tier = recommendedTier(ramGB: ramGB)
        return ModelTierInfo.catalog.first(where: { $0.tier == tier })?.id ?? "qwen35-2b-base-q4km"
    }

    var body: some View {
        if state.downloadPhase == .idle {
            tierPicker
        } else {
            downloadProgress
        }
    }

    private var tierPicker: some View {
        VStack(spacing: 16) {
            Text("Choose Your Model")
                .font(.title2.bold())
            Text("Your Mac has \(ramGB) GB RAM.  Recommended tier is highlighted.")
                .foregroundStyle(.secondary)
                .font(.callout)
                .multilineTextAlignment(.center)

            ScrollView {
                VStack(spacing: 8) {
                    ForEach(ModelTierInfo.catalog) { tier in
                        TierRow(
                            tier: tier,
                            isSelected: state.selectedTierId == tier.id,
                            isRecommended: tier.id == recommendedId
                        ) {
                            state.selectedTierId = tier.id
                        }
                    }
                }
                .padding(.horizontal, 2)
            }
            .frame(maxHeight: 260)
        }
        .padding(.horizontal, 32)
        .padding(.top, 20)
        .onReceive(NotificationCenter.default.publisher(for: .downloadProgressUpdated)) { note in
            handleProgressNote(note)
        }
    }

    private var downloadProgress: some View {
        VStack(spacing: 24) {
            downloadIcon.animation(.spring, value: iconName)
            VStack(spacing: 8) {
                Text(titleText).font(.title2.bold()).animation(.default, value: titleText)
                let tierName = ModelTierInfo.catalog.first(where: { $0.id == state.selectedTierId })
                    .map { "\($0.displayName) · \($0.sizeLabel)" } ?? ""
                Text("\(tierName)\nRuns entirely on your Mac.")
                    .multilineTextAlignment(.center).foregroundStyle(.secondary).frame(maxWidth: 400)
            }
            downloadProgressView
        }
        .padding(.horizontal, 40).padding(.vertical, 32)
        .onReceive(NotificationCenter.default.publisher(for: .downloadProgressUpdated)) { note in
            handleProgressNote(note)
        }
    }

    private var iconName: String {
        switch state.downloadPhase {
        case .complete: return "checkmark.circle.fill"
        case .failed:   return "xmark.circle.fill"
        default:        return "arrow.down.circle.fill"
        }
    }

    private var iconColor: Color {
        switch state.downloadPhase {
        case .complete: return .green
        case .failed:   return .red
        default:        return .blue
        }
    }

    private var titleText: String {
        switch state.downloadPhase {
        case .idle:         return "Download English Model"
        case .downloading:  return "Downloading…"
        case .verifying:    return "Verifying…"
        case .complete:     return "Download Complete"
        case .failed:       return "Download Failed"
        }
    }

    @ViewBuilder
    private var downloadIcon: some View {
        ZStack {
            if case .downloading = state.downloadPhase {
                Circle()
                    .stroke(Color.accentColor.opacity(0.15), lineWidth: 4)
                    .frame(width: 80, height: 80)
                Circle()
                    .trim(from: 0, to: state.downloadFraction)
                    .stroke(Color.accentColor as Color, style: StrokeStyle(lineWidth: 4, lineCap: .round))
                    .rotationEffect(.degrees(-90))
                    .frame(width: 80, height: 80)
                    .animation(.linear(duration: 0.3), value: state.downloadFraction)
            }
            Image(systemName: iconName)
                .font(.system(size: 52))
                .foregroundStyle(iconColor)
        }
        .frame(width: 80, height: 80)
    }

    @ViewBuilder
    private var downloadProgressView: some View {
        switch state.downloadPhase {
        case .idle:
            Text("Click 'Download Model' below to begin.")
                .font(.callout)
                .foregroundStyle(.secondary)

        case .downloading:
            VStack(spacing: 10) {
                ProgressView(value: state.downloadFraction)
                    .progressViewStyle(.linear)
                    .frame(maxWidth: 360)
                    .tint(.accentColor)

                HStack {
                    Text(state.downloadLabel)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Text("\(Int(state.downloadFraction * 100))%")
                        .font(.callout.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: 360)
            }

        case .verifying:
            HStack(spacing: 10) {
                ProgressView()
                    .scaleEffect(0.8)
                Text("Verifying checksum and signature…")
                    .foregroundStyle(.secondary)
            }

        case .complete:
            Label("Model verified and ready", systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .font(.callout.weight(.medium))

        case .failed(let msg):
            VStack(spacing: 6) {
                Label("Download failed", systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                Text(msg)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 360)
            }
        }
    }

    private func handleProgressNote(_ note: Notification) {
        guard let info = note.userInfo else { return }

        if let phase = info["phase"] as? String, phase == "verifying" {
            state.downloadPhase = .verifying
            return
        }
        if let phase = info["phase"] as? String, phase == "complete" {
            state.downloadPhase = .complete
            return
        }
        if let phase = info["phase"] as? String, phase == "failed",
           let err = info["error"] as? String {
            state.downloadPhase = .failed(err)
            return
        }

        if let downloaded = info["downloaded"] as? Int64,
           let total = info["total"] as? Int64 {
            state.downloadedBytes = downloaded
            state.totalBytes = total
            state.downloadPhase = .downloading
        } else if let fraction = info["progress"] as? Double {
            // Fallback: fraction only
            let total: Int64 = 986_000_000
            state.downloadedBytes = Int64(fraction * Double(total))
            state.totalBytes = total
            state.downloadPhase = fraction >= 1.0 ? .complete : .downloading
        }
    }
}

struct TierRow: View {
    let tier: ModelTierInfo
    let isSelected: Bool
    let isRecommended: Bool
    let onSelect: () -> Void

    var body: some View {
        Button(action: onSelect) {
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(tier.displayName).font(.body.weight(.medium))
                        if isRecommended {
                            Text("Recommended")
                                .font(.caption2.weight(.semibold))
                                .padding(.horizontal, 6).padding(.vertical, 2)
                                .background(Color.accentColor.opacity(0.15), in: Capsule())
                                .foregroundStyle(Color.accentColor)
                        }
                    }
                    HStack(spacing: 8) {
                        Text(tier.sizeLabel).font(.caption).foregroundStyle(.secondary)
                        Text("·").foregroundStyle(.tertiary)
                        Text(tier.ramLabel).font(.caption).foregroundStyle(.secondary)
                    }
                }
                Spacer()
                Image(systemName: isSelected ? "checkmark.circle.fill" : "circle")
                    .font(.title3)
                    .foregroundStyle(isSelected ? Color.accentColor : Color.secondary)
            }
            .padding(.horizontal, 14).padding(.vertical, 10)
            .background(
                RoundedRectangle(cornerRadius: 10)
                    .fill(isSelected ? Color.accentColor.opacity(0.08) : Color.secondary.opacity(0.05))
                    .overlay(
                        RoundedRectangle(cornerRadius: 10)
                            .stroke(isSelected ? Color.accentColor.opacity(0.4) : .clear, lineWidth: 1.5)
                    )
            )
        }
        .buttonStyle(.plain)
    }
}

struct DoneStep: View {
    var body: some View {
        VStack(spacing: 24) {
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 64))
                .foregroundStyle(.green)

            VStack(spacing: 10) {
                Text("You're all set!")
                    .font(.largeTitle.bold())
                Text("Start typing anywhere on your Mac.\nA ghost-text suggestion will appear after you pause — press Tab to accept, Escape to dismiss.")
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: 400)
            }

            HStack(spacing: 20) {
                KeyHint(key: "⇥ Tab", label: "Accept")
                KeyHint(key: "⎋ Esc", label: "Dismiss")
            }
        }
        .padding(.horizontal, 40)
        .padding(.vertical, 32)
    }
}

struct KeyHint: View {
    let key: String
    let label: String
    var body: some View {
        VStack(spacing: 4) {
            Text(key)
                .font(.system(.callout, design: .monospaced).bold())
                .padding(.horizontal, 14)
                .padding(.vertical, 7)
                .background(Color.secondary.opacity(0.12), in: RoundedRectangle(cornerRadius: 7))
            Text(label).font(.caption).foregroundStyle(.secondary)
        }
    }
}

// ── Model picker view (used by ModelPickerController) ─────────────────────────

struct ModelPickerView: View {
    @ObservedObject var state: OnboardingState
    let onDismiss: () -> Void

    private let ramGB = detectPhysicalRAMGB()
    private var recommendedId: String {
        let tier = recommendedTier(ramGB: ramGB)
        return ModelTierInfo.catalog.first(where: { $0.tier == tier })?.id ?? "qwen35-2b-base-q4km"
    }

    var body: some View {
        VStack(spacing: 0) {
            Group {
                if state.downloadPhase == .idle {
                    pickerBody
                } else {
                    progressBody
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)

            Divider()

            HStack {
                if state.downloadPhase != .complete {
                    Button("Cancel") { onDismiss() }
                }
                Spacer()
                actionButton
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 16)
        }
        .frame(width: 560, height: 440)
        .onChange(of: state.downloadPhase) { _, phase in
            if case .complete = phase {
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) { onDismiss() }
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: .downloadProgressUpdated)) { note in
            handleProgressNote(note)
        }
    }

    private var pickerBody: some View {
        VStack(spacing: 16) {
            Text("Choose Your Model")
                .font(.title2.bold())
            Text("Your Mac has \(ramGB) GB RAM. Recommended tier is highlighted.")
                .foregroundStyle(.secondary)
                .font(.callout)
                .multilineTextAlignment(.center)

            ScrollView {
                VStack(spacing: 8) {
                    ForEach(ModelTierInfo.catalog) { tier in
                        TierRow(
                            tier: tier,
                            isSelected: state.selectedTierId == tier.id,
                            isRecommended: tier.id == recommendedId
                        ) {
                            state.selectedTierId = tier.id
                        }
                    }
                }
                .padding(.horizontal, 2)
            }
            .frame(maxHeight: 300)
        }
        .padding(.horizontal, 32)
        .padding(.top, 24)
    }

    private var progressBody: some View {
        VStack(spacing: 24) {
            downloadIcon.animation(.spring, value: progressIconName)
            VStack(spacing: 8) {
                Text(progressTitle).font(.title2.bold())
                let tierName = ModelTierInfo.catalog.first(where: { $0.id == state.selectedTierId })
                    .map { "\($0.displayName) · \($0.sizeLabel)" } ?? ""
                Text("\(tierName)\nRuns entirely on your Mac.")
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: 400)
            }
            progressIndicator
        }
        .padding(.horizontal, 40)
        .padding(.vertical, 32)
    }

    private var progressIconName: String {
        switch state.downloadPhase {
        case .complete: return "checkmark.circle.fill"
        case .failed:   return "xmark.circle.fill"
        default:        return "arrow.down.circle.fill"
        }
    }

    private var progressTitle: String {
        switch state.downloadPhase {
        case .downloading: return "Downloading…"
        case .verifying:   return "Verifying…"
        case .complete:    return "Model Ready"
        case .failed:      return "Download Failed"
        default:           return ""
        }
    }

    @ViewBuilder
    private var downloadIcon: some View {
        ZStack {
            if case .downloading = state.downloadPhase {
                Circle()
                    .stroke(Color.accentColor.opacity(0.15), lineWidth: 4)
                    .frame(width: 80, height: 80)
                Circle()
                    .trim(from: 0, to: state.downloadFraction)
                    .stroke(Color.accentColor as Color, style: StrokeStyle(lineWidth: 4, lineCap: .round))
                    .rotationEffect(.degrees(-90))
                    .frame(width: 80, height: 80)
                    .animation(.linear(duration: 0.3), value: state.downloadFraction)
            }
            let iconColor: Color = {
                switch state.downloadPhase {
                case .complete: return .green
                case .failed:   return .red
                default:        return .blue
                }
            }()
            Image(systemName: progressIconName)
                .font(.system(size: 52))
                .foregroundStyle(iconColor)
        }
        .frame(width: 80, height: 80)
    }

    @ViewBuilder
    private var progressIndicator: some View {
        switch state.downloadPhase {
        case .downloading:
            VStack(spacing: 10) {
                ProgressView(value: state.downloadFraction)
                    .progressViewStyle(.linear)
                    .frame(maxWidth: 360)
                    .tint(.accentColor)
                HStack {
                    Text(state.downloadLabel).font(.callout).foregroundStyle(.secondary)
                    Spacer()
                    Text("\(Int(state.downloadFraction * 100))%")
                        .font(.callout.monospacedDigit()).foregroundStyle(.secondary)
                }
                .frame(maxWidth: 360)
            }
        case .verifying:
            HStack(spacing: 10) {
                ProgressView().scaleEffect(0.8)
                Text("Verifying checksum and signature…").foregroundStyle(.secondary)
            }
        case .complete:
            Label("Model verified and ready", systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green).font(.callout.weight(.medium))
        case .failed(let msg):
            VStack(spacing: 6) {
                Label("Download failed", systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                Text(msg).font(.caption).foregroundStyle(.secondary)
                    .multilineTextAlignment(.center).frame(maxWidth: 360)
            }
        default:
            EmptyView()
        }
    }

    @ViewBuilder
    private var actionButton: some View {
        switch state.downloadPhase {
        case .idle:
            Button("Switch Model") { startDownload() }
                .buttonStyle(.borderedProminent)
        case .downloading, .verifying:
            Button("Downloading…") {}
                .buttonStyle(.borderedProminent)
                .disabled(true)
        case .complete:
            Button("Done") { onDismiss() }
                .buttonStyle(.borderedProminent)
        case .failed:
            Button("Retry") { startDownload() }
                .buttonStyle(.borderedProminent)
        }
    }

    private func startDownload() {
        state.downloadPhase = .downloading
        IPCBridge.shared.notify(method: "startModelDownload", params: [
            "language": "en",
            "modelId": state.selectedTierId,
        ])
    }

    private func handleProgressNote(_ note: Notification) {
        guard let info = note.userInfo else { return }
        if let phase = info["phase"] as? String {
            switch phase {
            case "verifying": state.downloadPhase = .verifying
            case "complete":  state.downloadPhase = .complete
            case "failed":
                state.downloadPhase = .failed(info["error"] as? String ?? "Unknown error")
            default: break
            }
            return
        }
        if let downloaded = info["downloaded"] as? Int64, let total = info["total"] as? Int64 {
            state.downloadedBytes = downloaded
            state.totalBytes = total
            state.downloadPhase = .downloading
        } else if let fraction = info["progress"] as? Double {
            let total: Int64 = 986_000_000
            state.downloadedBytes = Int64(fraction * Double(total))
            state.totalBytes = total
            state.downloadPhase = fraction >= 1.0 ? .complete : .downloading
        }
    }
}
