import AppIntents

/// What Siri understands without any setup, and what Shortcuts, Spotlight
/// and the Action button offer. The devices in the phrases come from the
/// shared state (updated as devices change).
struct AudioNetShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: ListenIntent(),
            phrases: [
                "Listen to \(\.$device) with \(.applicationName)",
                "Listen to \(\.$device) in \(.applicationName)",
                "\(.applicationName) listen to \(\.$device)",
            ],
            shortTitle: "Listen to a Device",
            systemImageName: "headphones")
        AppShortcut(
            intent: SendMicrophoneIntent(),
            phrases: [
                "Send my microphone to \(\.$device) with \(.applicationName)",
                "\(.applicationName) send my microphone to \(\.$device)",
            ],
            shortTitle: "Send My Microphone",
            systemImageName: "mic")
        AppShortcut(
            intent: StopStreamsIntent(),
            phrases: [
                "Stop \(.applicationName)",
                "Stop \(.applicationName) streams",
                "Stop streaming with \(.applicationName)",
            ],
            shortTitle: "Stop Streams",
            systemImageName: "stop.circle")
        AppShortcut(
            intent: SetMutedIntent(value: true),
            phrases: ["Mute \(.applicationName)"],
            shortTitle: "Mute",
            systemImageName: "speaker.slash")
        AppShortcut(
            intent: SetMutedIntent(value: false),
            phrases: ["Unmute \(.applicationName)"],
            shortTitle: "Unmute",
            systemImageName: "speaker.wave.2")
        AppShortcut(
            intent: DeviceStatusIntent(),
            phrases: [
                "Is \(\.$device) online in \(.applicationName)",
                "Check \(\.$device) in \(.applicationName)",
            ],
            shortTitle: "Check a Device",
            systemImageName: "desktopcomputer")
    }
}

/// A Focus filter: during a Focus, AudioNet's notifications can arrive
/// quietly (in the notification list, without sound or lighting the
/// screen). When the Focus ends, they are as usual again.
struct AudioNetFocusFilter: SetFocusFilterIntent {
    static var title: LocalizedStringResource { "AudioNet Notifications" }
    static var description: IntentDescription? {
        IntentDescription("Deliver AudioNet's notifications quietly during this Focus.")
    }

    @Parameter(title: "Deliver Quietly")
    var quiet: Bool?

    init() {}

    var displayRepresentation: DisplayRepresentation {
        quiet == true ? "AudioNet notifications quietly" : "AudioNet notifications as usual"
    }

    func perform() async throws -> some IntentResult {
        // Ending the Focus calls this with nothing set: as usual again.
        let quiet = self.quiet ?? false
        SharedState.update { $0.quietNotifications = quiet }
        return .result()
    }
}
