import AppIntents
import Foundation

// AudioNet's actions for Siri, Shortcuts, Spotlight, the Action button,
// widgets, Control Center and the Live Activity's buttons.
//
// Compiled into the app and the widget extension. Actions that stream run
// in the app, where the engine is: iOS runs audio-playback and Live Activity
// intents there even when started from a widget or the Lock Screen. Reading
// a device's state runs anywhere (from the shared state file).

/// What the app does for an intent (set by the app at launch; never set in
/// the widget extension, which never runs these).
@MainActor
protocol AudioNetActionsHandler: AnyObject {
    /// Listens to a device's sound on this iPhone; says what started.
    func listen(to nodeId: String) async throws -> String
    /// Stops listening to a device; says what stopped.
    func stopListening(to nodeId: String) -> String
    /// Sends this iPhone's microphone to a device (the app is open).
    func sendMicrophone(to nodeId: String) async throws -> String
    func stopAll() -> String
    func setMuted(_ muted: Bool) -> String
    /// Shows a device in the app.
    func open(device nodeId: String)
}

@MainActor
enum AudioNetActions {
    static var handler: (any AudioNetActionsHandler)?

    static func run<T: Sendable>(_ body: @MainActor (any AudioNetActionsHandler) async throws -> T) async throws -> T {
        guard let handler else { throw AudioNetIntentError("Open AudioNet, then try again.") }
        return try await body(handler)
    }
}

/// A failed action, in words (Siri says it).
struct AudioNetIntentError: Error, CustomLocalizedStringResourceConvertible {
    let message: String
    init(_ message: String) { self.message = message }
    var localizedStringResource: LocalizedStringResource { "\(message)" }
}

// MARK: Devices

/// One of the accounts' devices, as Siri, Shortcuts and Spotlight know it.
struct DeviceEntity: AppEntity, Sendable {
    static var typeDisplayRepresentation: TypeDisplayRepresentation { "AudioNet Device" }
    static var defaultQuery: DeviceQuery { DeviceQuery() }

    /// The device's id (node id).
    var id: String
    var name: String
    /// Its state and account in words: "online, in alice on audionet.example.com".
    var detail: String

    init(id: String, name: String, detail: String) {
        self.id = id
        self.name = name
        self.detail = detail
    }

    init(_ d: SharedState.Device) {
        self.init(id: d.nodeId, name: d.name, detail: "\(d.stateText), in \(d.accountName)")
    }

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(title: "\(name)", subtitle: "\(detail)")
    }
}

@available(iOS 18.0, *)
extension DeviceEntity: IndexedEntity {}

/// Finds devices in the shared state (the app keeps it current).
struct DeviceQuery: EntityStringQuery {
    func entities(for identifiers: [String]) async throws -> [DeviceEntity] {
        let state = SharedState.load()
        return identifiers.compactMap { state.device($0).map(DeviceEntity.init) }
    }

    func entities(matching string: String) async throws -> [DeviceEntity] {
        let wanted = string.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return SharedState.load().devices
            .filter { wanted.isEmpty || $0.name.lowercased().contains(wanted) }
            .map(DeviceEntity.init)
    }

    func suggestedEntities() async throws -> [DeviceEntity] {
        SharedState.load().devices.map(DeviceEntity.init)
    }
}

// MARK: Streaming

/// Listens to a device's sound on this iPhone. Runs in the app, without
/// opening it (an audio-playback action may start audio from the Lock
/// Screen, a widget or Siri).
struct ListenIntent: AudioPlaybackIntent {
    static var title: LocalizedStringResource { "Listen to a Device" }
    static var description: IntentDescription {
        IntentDescription("Plays a device's sound on this phone with AudioNet: the sound chosen for it, or its default one.")
    }

    @Parameter(title: "Device")
    var device: DeviceEntity

    init() {}
    init(device: DeviceEntity) { self.device = device }

    static var parameterSummary: some ParameterSummary {
        Summary("Listen to \(\.$device)")
    }

    @MainActor
    func perform() async throws -> some IntentResult & ProvidesDialog {
        let text = try await AudioNetActions.run { try await $0.listen(to: device.id) }
        return .result(dialog: "\(text)")
    }
}

/// Sends this iPhone's microphone to a device. Opens AudioNet: iOS lets an
/// app start recording only while it is in front.
struct SendMicrophoneIntent: AppIntent {
    static var title: LocalizedStringResource { "Send My Microphone" }
    static var description: IntentDescription {
        IntentDescription("Sends this phone's microphone to a device with AudioNet. AudioNet opens to start recording.")
    }
    static var openAppWhenRun: Bool { true }

    @Parameter(title: "Device")
    var device: DeviceEntity

    init() {}
    init(device: DeviceEntity) { self.device = device }

    static var parameterSummary: some ParameterSummary {
        Summary("Send my microphone to \(\.$device)")
    }

    @MainActor
    func perform() async throws -> some IntentResult & ProvidesDialog {
        let text = try await AudioNetActions.run { try await $0.sendMicrophone(to: device.id) }
        return .result(dialog: "\(text)")
    }
}

/// Stops every stream this iPhone started.
struct StopStreamsIntent: LiveActivityIntent {
    static var title: LocalizedStringResource { "Stop AudioNet Streams" }
    static var description: IntentDescription {
        IntentDescription("Stops every stream this phone started with AudioNet.")
    }

    init() {}

    @MainActor
    func perform() async throws -> some IntentResult & ProvidesDialog {
        let text = try await AudioNetActions.run { $0.stopAll() }
        return .result(dialog: "\(text)")
    }
}

/// Mutes or unmutes every stream this iPhone started.
struct SetMutedIntent: SetValueIntent, LiveActivityIntent {
    static var title: LocalizedStringResource { "Mute AudioNet" }
    static var description: IntentDescription {
        IntentDescription("Mutes or unmutes every stream this phone started with AudioNet.")
    }

    @Parameter(title: "Muted")
    var value: Bool

    init() {}
    init(value: Bool) { self.value = value }

    static var parameterSummary: some ParameterSummary {
        Summary("Set AudioNet muted to \(\.$value)")
    }

    @MainActor
    func perform() async throws -> some IntentResult & ProvidesDialog {
        let text = try await AudioNetActions.run { $0.setMuted(value) }
        return .result(dialog: "\(text)")
    }
}

/// Listens to a device, or stops listening to it (a Control Center toggle).
struct ToggleListenIntent: SetValueIntent, AudioPlaybackIntent {
    static var title: LocalizedStringResource { "Listen to a Device or Stop" }
    static var isDiscoverable: Bool { false }

    @Parameter(title: "Device")
    var device: DeviceEntity?

    @Parameter(title: "Listening")
    var value: Bool

    init() {}
    init(device: DeviceEntity?) { self.device = device }

    @MainActor
    func perform() async throws -> some IntentResult {
        guard let device else { throw AudioNetIntentError("Choose a device for this control first.") }
        if value {
            _ = try await AudioNetActions.run { try await $0.listen(to: device.id) }
        } else {
            _ = try await AudioNetActions.run { $0.stopListening(to: device.id) }
        }
        return .result()
    }
}

// MARK: Reading state

/// Says whether a device is online and sharing (from what AudioNet last
/// knew: current while AudioNet runs, and updated by notifications).
struct DeviceStatusIntent: AppIntent {
    static var title: LocalizedStringResource { "Check a Device" }
    static var description: IntentDescription {
        IntentDescription("Says whether a device in your AudioNet accounts is online and sharing its audio.")
    }

    @Parameter(title: "Device")
    var device: DeviceEntity

    init() {}
    init(device: DeviceEntity) { self.device = device }

    static var parameterSummary: some ParameterSummary {
        Summary("Check \(\.$device)")
    }

    func perform() async throws -> some IntentResult & ReturnsValue<String> & ProvidesDialog {
        let state = SharedState.load()
        guard let d = state.device(device.id) else {
            throw AudioNetIntentError("AudioNet does not know \(device.name) any more. Open AudioNet to update its devices.")
        }
        var text = !d.online ? "\(d.name) is offline."
            : d.sharing ? "\(d.name) is online and sharing its audio."
            : "\(d.name) is online, not sharing its audio."
        if state.accountCount > 1 { text += " It is in \(d.accountName)." }
        if Date().timeIntervalSince(state.updated) > 15 * 60 {
            let when = state.updated.formatted(date: .omitted, time: .shortened)
            text += " As of \(when), when AudioNet last had news."
        }
        return .result(value: text, dialog: "\(text)")
    }
}

/// Opens a device in AudioNet (Spotlight results, Shortcuts).
struct OpenDeviceIntent: OpenIntent {
    static var title: LocalizedStringResource { "Open a Device" }

    @Parameter(title: "Device")
    var target: DeviceEntity

    init() {}
    init(target: DeviceEntity) { self.target = target }

    @MainActor
    func perform() async throws -> some IntentResult {
        try await AudioNetActions.run { $0.open(device: target.id) }
        return .result()
    }
}

// MARK: Widget and control configuration

/// The device a widget shows.
struct SelectDeviceIntent: WidgetConfigurationIntent {
    static var title: LocalizedStringResource { "Choose a Device" }
    static var description: IntentDescription { IntentDescription("The AudioNet device this widget shows.") }

    @Parameter(title: "Device")
    var device: DeviceEntity?

    init() {}
}

/// The device a Control Center control listens to.
@available(iOS 18.0, *)
struct SelectListenDeviceIntent: ControlConfigurationIntent {
    static var title: LocalizedStringResource { "Choose a Device to Listen To" }

    @Parameter(title: "Device")
    var device: DeviceEntity?

    init() {}
}
