import AppIntents
import SwiftUI
import WidgetKit

// Control Center, Lock Screen and Action button controls (iOS 18 and
// later). Each runs its action in the app; their states come from the
// shared state the app keeps current.

/// Stops every stream this iPhone started.
@available(iOS 18.0, *)
struct StopStreamsControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: "AudioNetStop") {
            ControlWidgetButton(action: StopStreamsIntent()) {
                Label("Stop AudioNet Streams", systemImage: "stop.circle")
            }
        }
        .displayName("Stop AudioNet Streams")
        .description("Stops every stream this iPhone started.")
    }
}

/// Mutes or unmutes every stream this iPhone started.
@available(iOS 18.0, *)
struct MuteControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: "AudioNetMute", provider: MutedValue()) { muted in
            ControlWidgetToggle("Mute AudioNet", isOn: muted, action: SetMutedIntent()) { on in
                Label(on ? "Muted" : "Not Muted", systemImage: on ? "speaker.slash.fill" : "speaker.wave.2.fill")
            }
        }
        .displayName("Mute AudioNet")
        .description("Mutes or unmutes every stream this iPhone started.")
    }
}

@available(iOS 18.0, *)
struct MutedValue: ControlValueProvider {
    var previewValue: Bool { false }
    func currentValue() async throws -> Bool { SharedState.load().allMuted }
}

/// Listens to a chosen device, or stops listening to it.
@available(iOS 18.0, *)
struct ListenControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        AppIntentControlConfiguration(kind: "AudioNetListen", provider: ListenValue()) { value in
            ControlWidgetToggle(isOn: value.listening,
                                action: ToggleListenIntent(device: value.device)) {
                Label(value.device.map { "Listen to \($0.name)" } ?? "Choose a Device",
                      systemImage: "headphones")
            } valueLabel: { on in
                Text(on ? "Listening" : value.state)
            }
        }
        .displayName("Listen to a Device")
        .description("Listens to the device you choose, or stops listening to it.")
        .promptsForUserConfiguration()
    }
}

@available(iOS 18.0, *)
struct ListenValue: AppIntentControlValueProvider {
    struct Value {
        var device: DeviceEntity?
        var listening: Bool
        /// The device's state in words, for the control's label.
        var state: String
    }

    func previewValue(configuration: SelectListenDeviceIntent) -> Value {
        Value(device: configuration.device, listening: false, state: "online")
    }

    func currentValue(configuration: SelectListenDeviceIntent) async throws -> Value {
        let state = SharedState.load()
        guard let entity = configuration.device else {
            return Value(device: nil, listening: false, state: "No device chosen")
        }
        let d = state.device(entity.id)
        return Value(device: entity, listening: state.isListening(to: entity.id),
                     state: d?.stateText.capitalized ?? "Not in your accounts")
    }
}
