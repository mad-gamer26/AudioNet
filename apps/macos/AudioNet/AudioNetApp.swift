import SwiftUI

@main
struct AudioNetApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var model = AppModel()

    var body: some Scene {
        Window("AudioNet", id: "main") {
            RootView()
                .environmentObject(model)
                .frame(minWidth: 560, minHeight: 640)
                .onAppear { delegate.model = model }
        }
        .windowResizability(.contentMinSize)
        .suppressedAtLaunch(AppDelegate.startsInMenuBar)

        // The menu bar item keeps AudioNet reachable when its window is
        // closed (like the Windows app's tray icon). VoiceOver reaches it
        // with VO-M M.
        // Both observe the model themselves: a value handed down from here
        // (or an environment object) is not refreshed in the menu bar item
        // when AudioNet goes online or offline, above all with no window
        // open.
        MenuBarExtra {
            MenuBarContent(model: model)
        } label: {
            MenuBarLabel(model: model, delegate: delegate)
        }

        Settings {
            SettingsView(updater: model.updater).environmentObject(model)
        }
    }
}

extension Scene {
    /// Opens the window at launch unless `suppressed` (macOS 15 and later;
    /// earlier, the app delegate closes it right after launch).
    func suppressedAtLaunch(_ suppressed: Bool) -> some Scene {
        if #available(macOS 15.0, *) {
            return defaultLaunchBehavior(suppressed ? .suppressed : .automatic)
        } else {
            return self
        }
    }
}

/// The Dock icon follows the window: while AudioNet runs only in the menu
/// bar (window closed, "keep in the menu bar" on, or started there) it has
/// no Dock icon and is not in the Command-Tab switcher; opening a window
/// brings the Dock icon back.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    weak var model: AppModel?
    /// Opens the main window (SwiftUI's openWindow, handed over by the menu
    /// bar icon, which exists from launch even when the window does not).
    var openMain: (() -> Void)?

    private static var settings: UserDefaults { AccountStore.settings }

    /// Started from the menu bar only: the option, or after an update when
    /// the window was closed.
    static var startsInMenuBar: Bool {
        settings.bool(forKey: "startInMenuBar") || UserDefaults.standard.bool(forKey: "AudioNetStartHidden")
    }

    private var keepInMenuBar: Bool {
        Self.settings.object(forKey: "keepInMenuBar") as? Bool ?? true
    }

    func applicationWillFinishLaunching(_ notification: Notification) {
        if Self.startsInMenuBar { NSApp.setActivationPolicy(.accessory) }
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let center = NotificationCenter.default
        center.addObserver(self, selector: #selector(windowWillClose(_:)),
                           name: NSWindow.willCloseNotification, object: nil)
        center.addObserver(self, selector: #selector(windowDidBecomeKey(_:)),
                           name: NSWindow.didBecomeKeyNotification, object: nil)
        if Self.startsInMenuBar {
            // macOS 14 opens the window anyway.
            DispatchQueue.main.async {
                for w in NSApp.windows where Self.isMain(w) { w.close() }
                NSApp.setActivationPolicy(.accessory)
            }
        }
    }

    /// Opening AudioNet again while it runs (Spotlight, Launchpad, the
    /// Finder, the Dock) shows its window, also when it started in the menu
    /// bar and never had one.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        guard let openMain, !NSApp.windows.contains(where: { Self.isMain($0) && $0.isVisible }) else { return true }
        NSApp.setActivationPolicy(.regular)
        openMain()
        NSApp.activate()
        return false
    }

    /// Closing the window keeps AudioNet running in the menu bar, unless
    /// that option is off.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        !keepInMenuBar
    }

    private static func isMain(_ w: NSWindow) -> Bool {
        w.identifier?.rawValue.hasPrefix("main") == true
    }

    /// A titled window the person can see (the main window or Settings).
    private static func isOrdinary(_ w: NSWindow) -> Bool {
        w.isVisible && w.styleMask.contains(.titled)
    }

    @objc private func windowWillClose(_ note: Notification) {
        guard let closing = note.object as? NSWindow, Self.isOrdinary(closing), keepInMenuBar else { return }
        // After the close completes: hide from the Dock if no window is left.
        DispatchQueue.main.async {
            guard !NSApp.windows.contains(where: { $0 !== closing && Self.isOrdinary($0) }) else { return }
            NSApp.setActivationPolicy(.accessory)
            if Self.isMain(closing) {
                AccessibilityNotification.Announcement("AudioNet is still running in the menu bar.").post()
            }
        }
    }

    @objc private func windowDidBecomeKey(_ note: Notification) {
        guard let w = note.object as? NSWindow, w.styleMask.contains(.titled),
              NSApp.activationPolicy() != .regular else { return }
        NSApp.setActivationPolicy(.regular)
        NSApp.activate()
    }
}

/// The menu bar icon: filled while sharing (in any account). VoiceOver
/// reads "AudioNet, sharing" or "AudioNet, not sharing".
struct MenuBarLabel: View {
    @ObservedObject var model: AppModel
    let delegate: AppDelegate
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        let sharing = !model.sharingAccounts.isEmpty
        Label("AudioNet", systemImage: sharing ? "waveform.circle.fill" : "waveform.circle")
            .accessibilityLabel(sharing ? "AudioNet, sharing" : "AudioNet, not sharing")
            .onAppear { delegate.openMain = { openWindow(id: "main") } }
    }
}

struct MenuBarContent: View {
    @ObservedObject var model: AppModel
    @Environment(\.openWindow) private var openWindow
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        Text(!model.signedIn ? "AudioNet is not signed in"
             : model.sharingAccounts.isEmpty ? "Online, not sharing this Mac's audio" : "Online, sharing this Mac's audio")
        Button("Open AudioNet") {
            NSApp.setActivationPolicy(.regular)
            openWindow(id: "main")
            NSApp.activate()
        }
        if model.signedIn {
            // Every account at once.
            Button(model.sharingAccounts.isEmpty ? "Start Sharing" : "Stop Sharing") {
                model.toggleSharingEverywhere()
            }
        }
        // With no window and no Dock icon, this is the way to Settings.
        Button("Settings…") {
            NSApp.setActivationPolicy(.regular)
            openSettings()
            NSApp.activate()
        }
        .keyboardShortcut(",")
        if model.updater.configured {
            Button("Check for Updates") { Task { await model.updater.check(userAsked: true) } }
        }
        Divider()
        Button("Quit AudioNet") { NSApp.terminate(nil) }
            .keyboardShortcut("q")
    }
}
