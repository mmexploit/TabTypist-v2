import AppKit
import SwiftUI

// Menu bar status item with active/paused states.
final class MenuBarController: NSObject {
    static let shared = MenuBarController()

    private var statusItem: NSStatusItem?
    private var currentApp: String = ""
    private var isActive: Bool = true
    private var loadedModelLabel: String = "Loading model…"

    func setup() {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        updateIcon()

        let menu = NSMenu()
        menu.delegate = self
        statusItem?.menu = menu
    }

    func update(appName: String, active: Bool) {
        currentApp = appName
        isActive = active
        DispatchQueue.main.async { self.updateIcon() }
    }

    func modelLoaded(tier: String, displayName: String) {
        let branded = ModelTierInfo.brandedName(for: tier)
        loadedModelLabel = branded
    }

    private func updateIcon() {
        let name = isActive ? "t.square.fill" : "t.square"
        statusItem?.button?.image = NSImage(
            systemSymbolName: name,
            accessibilityDescription: isActive ? "TabTypist active" : "TabTypist paused"
        )
        statusItem?.button?.image?.isTemplate = true
    }
}

extension MenuBarController: NSMenuDelegate {
    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()

        // Model display at the top.
        let modelItem = NSMenuItem(title: "Model: \(loadedModelLabel)", action: nil, keyEquivalent: "")
        modelItem.isEnabled = false
        menu.addItem(modelItem)

        let changeModelItem = NSMenuItem(title: "Change model…", action: #selector(openModelPicker), keyEquivalent: "")
        changeModelItem.target = self
        menu.addItem(changeModelItem)
        menu.addItem(.separator())

        let appLabel = currentApp.isEmpty ? "No focused app" : currentApp
        let stateLabel = isActive ? "Active" : "Paused"
        menu.addItem(NSMenuItem(
            title: "\(appLabel) — \(stateLabel)",
            action: nil,
            keyEquivalent: ""
        ))

        menu.addItem(.separator())

        let toggleTitle = isActive
            ? "Disable in \(currentApp.isEmpty ? "This App" : currentApp)"
            : "Enable in \(currentApp.isEmpty ? "This App" : currentApp)"
        let toggleItem = NSMenuItem(title: toggleTitle, action: #selector(toggleCurrentApp), keyEquivalent: "")
        toggleItem.target = self
        menu.addItem(toggleItem)

        menu.addItem(.separator())

        let settingsItem = NSMenuItem(title: "Settings…", action: #selector(openSettings), keyEquivalent: ",")
        settingsItem.target = self
        menu.addItem(settingsItem)

        let updatesItem = NSMenuItem(title: "Check for Updates…", action: #selector(checkForUpdates), keyEquivalent: "")
        updatesItem.target = self
        menu.addItem(updatesItem)

        menu.addItem(.separator())

        let quitItem = NSMenuItem(title: "Quit TabTypist", action: #selector(NSApp.terminate(_:)), keyEquivalent: "q")
        menu.addItem(quitItem)
    }

    @objc private func toggleCurrentApp() {
        if isActive {
            IPCBridge.shared.notify(method: "updateSetting", params: [
                "key": "disableApp",
                "bundleId": currentApp,
            ])
        } else {
            IPCBridge.shared.notify(method: "updateSetting", params: [
                "key": "enableApp",
                "bundleId": currentApp,
            ])
        }
    }

    @objc private func openModelPicker() {
        ModelPickerController.shared.show()
    }

    @objc private func openSettings() {
        SettingsWindowController.shared.show()
    }

    @objc private func checkForUpdates() {
        NotificationCenter.default.post(name: .checkForUpdatesRequested, object: nil)
    }
}
