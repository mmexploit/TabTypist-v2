import AppKit
import SwiftUI
import IOKit.hid

final class SettingsWindowController: NSObject {
    static let shared = SettingsWindowController()

    private var window: NSWindow?

    func show() {
        if let w = window, w.isVisible {
            NSApp.activate(ignoringOtherApps: true)
            w.makeKeyAndOrderFront(nil)
            return
        }

        let hosting = NSHostingView(rootView: SettingsView())
        hosting.autoresizingMask = [.width, .height]

        let w = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 540, height: 740),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        w.title = "TabTypist Settings"
        w.contentMinSize = NSSize(width: 500, height: 480)
        hosting.frame = w.contentLayoutRect
        w.contentView = hosting
        w.center()
        NSApp.activate(ignoringOtherApps: true)
        w.makeKeyAndOrderFront(nil)
        window = w
    }
}

struct SettingsView: View {
    // Model
    @State private var activeModelTier: String =
        UserDefaults.standard.string(forKey: "activeModelTier") ?? ""

    // Completion behavior
    @State private var completionLength: String =
        UserDefaults.standard.string(forKey: "completionLength") ?? "long"
    @State private var multiLineEnabled: Bool =
        UserDefaults.standard.bool(forKey: "multiLineEnabled")

    // Permissions
    @State private var axGranted: Bool = AXIsProcessTrusted()
    @State private var screenGranted: Bool = CGPreflightScreenCaptureAccess()
    @State private var inputMonGranted: Bool =
        IOHIDCheckAccess(kIOHIDRequestTypeListenEvent) == kIOHIDAccessTypeGranted

    // Context
    @State private var clipboardEnabled: Bool =
        UserDefaults.standard.bool(forKey: "clipboardContextEnabled")

    // Writing
    @State private var userName: String =
        UserDefaults.standard.string(forKey: "userName") ?? ""
    @State private var customRulesGlobal: String =
        UserDefaults.standard.string(forKey: "customRulesGlobal") ?? ""

    // Model downloads
    @State private var hfToken: String =
        UserDefaults.standard.string(forKey: "hfToken") ?? ""
    @State private var customModelUrl: String = ""
    @State private var downloadingCustom: Bool = false

    // Privacy
    @State private var telemetryEnabled: Bool =
        UserDefaults.standard.bool(forKey: "telemetryEnabled")

    // Danger zone
    @State private var showResetConfirm: Bool = false

    var body: some View {
        Form {
            // ── Model ─────────────────────────────────────────────────────────
            Section("Model") {
                LabeledContent("Active model") {
                    HStack(spacing: 10) {
                        if !activeModelTier.isEmpty {
                            Text(ModelTierInfo.brandedName(for: activeModelTier))
                                .foregroundStyle(.secondary)
                        }
                        Button("Change model…") {
                            ModelPickerController.shared.show()
                        }
                    }
                }

                LabeledContent("HuggingFace token") {
                    SecureField("hf_... (optional)", text: $hfToken)
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: 220)
                        .onSubmit { sendHfToken() }
                }
                Text("Not needed for built-in models — they're on public repos. Only required for custom GGUFs from gated or private HuggingFace repos.")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                VStack(alignment: .leading, spacing: 8) {
                    Text("Custom GGUF")
                        .font(.subheadline.weight(.medium))
                    TextField("HuggingFace GGUF URL", text: $customModelUrl)
                        .textFieldStyle(.roundedBorder)
                    HStack {
                        Text("Signature verification is skipped for custom models.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Spacer()
                        Button(downloadingCustom ? "Downloading…" : "Download") {
                            downloadCustomModel()
                        }
                        .disabled(customModelUrl.isEmpty || downloadingCustom)
                    }
                }
            }

            // ── Completion Behavior ───────────────────────────────────────────
            Section("Completion Behavior") {
                LabeledContent("Length") {
                    Picker("", selection: $completionLength) {
                        Text("Short").tag("short")
                        Text("Medium").tag("medium")
                        Text("Long").tag("long")
                    }
                    .pickerStyle(.segmented)
                    .frame(maxWidth: 200)
                    .onChange(of: completionLength) { _, value in
                        UserDefaults.standard.set(value, forKey: "completionLength")
                        IPCBridge.shared.notify(method: "updateSetting", params: [
                            "key": "completionLength", "value": value,
                        ])
                    }
                }
                Text("Short: ~11 tokens. Medium: ~18. Long: ~30.")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                Toggle("Allow multi-paragraph completions", isOn: $multiLineEnabled)
                    .onChange(of: multiLineEnabled) { _, enabled in
                        UserDefaults.standard.set(enabled, forKey: "multiLineEnabled")
                        IPCBridge.shared.notify(method: "updateSetting", params: [
                            "key": "multiLineEnabled", "value": enabled,
                        ])
                    }
                Text("Stops at blank lines when off. Token budget doubles (capped at 60) when on.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            // ── Permissions ───────────────────────────────────────────────────
            Section("Permissions") {
                permissionRow(
                    name: "Accessibility", granted: axGranted,
                    detail: "Read caret position and insert completions when you press Tab."
                ) {
                    let opts = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true]
                    _ = AXIsProcessTrustedWithOptions(opts as CFDictionary)
                    openPrivacyPane("Privacy_Accessibility")
                }
                permissionRow(
                    name: "Input Monitoring", granted: inputMonGranted,
                    detail: "Detect the Tab key so suggestions can be accepted."
                ) {
                    _ = IOHIDRequestAccess(kIOHIDRequestTypeListenEvent)
                    openPrivacyPane("Privacy_ListenEvent")
                }
                permissionRow(
                    name: "Screen Recording", granted: screenGranted,
                    detail: "Optional. On-device OCR of nearby text for context-aware suggestions."
                ) {
                    _ = CGRequestScreenCaptureAccess()
                    openPrivacyPane("Privacy_ScreenCapture")
                }
                if !screenGranted {
                    Text("After enabling Screen Recording, macOS may ask you to quit & reopen TabTypist.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            // ── Context ───────────────────────────────────────────────────────
            Section("Context") {
                Toggle("Include clipboard text in suggestions", isOn: $clipboardEnabled)
                    .onChange(of: clipboardEnabled) { _, enabled in
                        UserDefaults.standard.set(enabled, forKey: "clipboardContextEnabled")
                        IPCBridge.shared.notify(method: "updateSetting", params: [
                            "key": "clipboardContextEnabled", "value": enabled,
                        ])
                    }
                Text("TabTypist reads your clipboard to offer more relevant completions. Nothing leaves your device.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            // ── Writing ───────────────────────────────────────────────────────
            Section("Writing") {
                LabeledContent("Your name") {
                    TextField("Used in suggestions", text: $userName)
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: 220)
                        .onSubmit { sendUserName() }
                }
                Text("Shown to the model as context for more relevant suggestions.")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                VStack(alignment: .leading, spacing: 6) {
                    Text("Global rules")
                        .font(.subheadline.weight(.medium))
                    TextEditor(text: $customRulesGlobal)
                        .font(.body)
                        .scrollContentBackground(.hidden)
                        .padding(6)
                        .frame(height: 80)
                        .background(Color(nsColor: .textBackgroundColor))
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                        .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color.secondary.opacity(0.25)))
                        .onChange(of: customRulesGlobal) { _, newValue in
                            sendCustomRulesGlobal(newValue)
                        }
                    Text("Applied to all apps. Example: use formal tone, prefer short sentences, write in British English.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            // ── Updates ───────────────────────────────────────────────────────
            Section("Updates") {
                Button("Check for Updates…") {
                    NotificationCenter.default.post(name: .checkForUpdatesRequested, object: nil)
                }
            }

            // ── Privacy ───────────────────────────────────────────────────────
            Section("Privacy") {
                Toggle("Send anonymous usage data", isOn: $telemetryEnabled)
                    .onChange(of: telemetryEnabled) { _, enabled in
                        UserDefaults.standard.set(enabled, forKey: "telemetryEnabled")
                        IPCBridge.shared.notify(method: "updateSetting", params: [
                            "key": "telemetryEnabled", "value": enabled,
                        ])
                    }
                Text("Never includes your text, completions, or identity. Only: model tier, accept/dismiss counts, app version.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            // ── About ─────────────────────────────────────────────────────────
            Section("About") {
                LabeledContent("Version") {
                    Text(Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "—")
                        .foregroundStyle(.secondary)
                }
                LabeledContent("License") {
                    Text("Functional Source License 1.1")
                        .foregroundStyle(.secondary)
                }
                Button("Reset TabTypist…", role: .destructive) { showResetConfirm = true }
            }
        }
        .formStyle(.grouped)
        .confirmationDialog(
            "This removes all models, settings, and stored data.",
            isPresented: $showResetConfirm,
            titleVisibility: .visible
        ) {
            Button("Reset", role: .destructive) {
                IPCBridge.shared.notify(method: "resetTabTypist", params: [:])
            }
            Button("Cancel", role: .cancel) {}
        }
        .onReceive(
            NotificationCenter.default.publisher(for: .downloadProgressUpdated)
        ) { note in
            if let phase = note.userInfo?["phase"] as? String {
                downloadingCustom = (phase == "downloading" || phase == "verifying")
                if phase == "complete" || phase == "failed" { customModelUrl = "" }
            }
        }
        .onReceive(Timer.publish(every: 1.5, on: .main, in: .common).autoconnect()) { _ in
            refreshPermissions()
        }
        .onAppear {
            activeModelTier = UserDefaults.standard.string(forKey: "activeModelTier") ?? ""
        }
    }

    // ── Permission row ─────────────────────────────────────────────────────────

    @ViewBuilder
    private func permissionRow(
        name: String, granted: Bool, detail: String, grant: @escaping () -> Void
    ) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: granted ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                .foregroundStyle(granted ? .green : .orange)
            VStack(alignment: .leading, spacing: 2) {
                Text(name)
                Text(detail).font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            if granted {
                Text("Granted").font(.caption).foregroundStyle(.secondary)
            } else {
                Button("Grant…", action: grant)
            }
        }
        .padding(.vertical, 2)
    }

    private func refreshPermissions() {
        axGranted = AXIsProcessTrusted()
        screenGranted = CGPreflightScreenCaptureAccess()
        inputMonGranted = IOHIDCheckAccess(kIOHIDRequestTypeListenEvent) == kIOHIDAccessTypeGranted
    }

    private func openPrivacyPane(_ anchor: String) {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(anchor)") {
            NSWorkspace.shared.open(url)
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    private func sendUserName() {
        let trimmed = userName.trimmingCharacters(in: .whitespaces)
        UserDefaults.standard.set(trimmed, forKey: "userName")
        IPCBridge.shared.notify(method: "updateSetting", params: [
            "key": "userName", "value": trimmed,
        ])
    }

    private func sendHfToken() {
        let trimmed = hfToken.trimmingCharacters(in: .whitespaces)
        UserDefaults.standard.set(trimmed, forKey: "hfToken")
        IPCBridge.shared.notify(method: "updateSetting", params: [
            "key": "hfToken", "value": trimmed,
        ])
    }

    private func sendCustomRulesGlobal(_ text: String) {
        UserDefaults.standard.set(text, forKey: "customRulesGlobal")
        IPCBridge.shared.notify(method: "updateSetting", params: [
            "key": "customRulesGlobal", "value": text,
        ])
    }

    private func downloadCustomModel() {
        let url = customModelUrl.trimmingCharacters(in: .whitespaces)
        guard !url.isEmpty else { return }
        downloadingCustom = true
        IPCBridge.shared.notify(method: "startModelDownload", params: [
            "language": "en",
            "customUrl": url,
        ])
    }
}
