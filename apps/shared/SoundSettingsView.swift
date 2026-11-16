import SwiftUI

/// Another device's sound settings: each output's and microphone's volume,
/// mute and default, devices turned off and on, and a phone's own volumes.
/// `device` is the live one (from the device list, so changes made at the
/// device or elsewhere show at once); `nil` once it is gone. `change` asks
/// the device (the answer comes as a device update, or a refusal said by
/// the app).
struct SoundSettingsView: View {
    let device: Device?
    let change: (SoundChange) -> Void

    var body: some View {
        Form {
            if let d = device {
                if !d.allowsControl {
                    Text("\(d.name) doesn't let other devices change its sound settings.")
                        .accessibilityIdentifier("controlOff")
                } else {
                    content(d)
                }
            } else {
                Text("This device is no longer visible.")
            }
        }
    }

    @ViewBuilder private func content(_ d: Device) -> some View {
        let outputs = d.outputs.filter { $0.control != nil }
        let microphones = d.sources.filter { $0.isInput && $0.control != nil }
        if outputs.isEmpty && microphones.isEmpty && d.volumes.isEmpty && d.disabled.isEmpty {
            Text(d.platform == "ios"
                 ? "An iPhone's sound settings can't be changed from other devices."
                 : "\(d.name) doesn't report its sound settings. It may need a newer AudioNet.")
        }
        if !outputs.isEmpty {
            Section {
                ForEach(outputs, id: \.id) { o in
                    EndpointSettings(name: o.name, target: o.id, isDefault: o.isDefault,
                                     control: o.control!, change: change)
                }
            } header: {
                Text("Outputs").accessibilityAddTraits(.isHeader)
            }
        }
        if !microphones.isEmpty {
            Section {
                ForEach(microphones, id: \.id) { m in
                    EndpointSettings(name: m.name, target: m.id, isDefault: m.isDefault,
                                     control: m.control!, change: change)
                }
            } header: {
                Text("Microphones").accessibilityAddTraits(.isHeader)
            }
        }
        if !d.volumes.isEmpty {
            Section {
                ForEach(d.volumes, id: \.id) { v in
                    VolumeSlider(name: v.name, value: v.volume) { change(.volume(target: v.id, volume: $0)) }
                    if v.canMute, let muted = v.muted {
                        Toggle("Mute \(v.name.lowercased())", isOn: Binding(
                            get: { muted }, set: { change(.mute(target: v.id, muted: $0)) }))
                    }
                }
            } header: {
                Text("Volumes").accessibilityAddTraits(.isHeader)
            }
        }
        if !d.disabled.isEmpty {
            Section {
                ForEach(d.disabled, id: \.id) { x in
                    LabeledContent("\(x.name) (\(x.isInput ? "microphone" : "output"), disabled)") {
                        Button("Enable") { change(.enabled(target: x.id, enabled: true)) }
                            .accessibilityLabel("Enable \(x.name)")
                    }
                }
            } header: {
                Text("Disabled").accessibilityAddTraits(.isHeader)
            }
        }
    }
}

/// One output's or microphone's settings.
private struct EndpointSettings: View {
    let name: String
    let target: String
    let isDefault: Bool
    let control: SoundControl
    let change: (SoundChange) -> Void

    var body: some View {
        let roles = [isDefault ? "default" : nil,
                     control.isDefaultCommunications ? "default for communications" : nil].compactMap { $0 }
        Text(roles.isEmpty ? name : "\(name) (\(roles.joined(separator: ", ")))")
            .font(.headline)
        if control.canSetVolume, let v = control.volume {
            VolumeSlider(name: name, value: v) { change(.volume(target: target, volume: $0)) }
        } else if let v = control.volume {
            LabeledContent("Volume", value: "\(Int((v * 100).rounded()))%")
        }
        if control.canMute, let muted = control.muted {
            Toggle("Mute \(name)", isOn: Binding(get: { muted }, set: { change(.mute(target: target, muted: $0)) }))
        }
        if control.canSetDefault && !isDefault {
            Button("Make \(name) the Default") { change(.default(target: target, communications: false)) }
        }
        if control.canSetDefaultCommunications && !control.isDefaultCommunications {
            Button("Make \(name) the Default for Communications") {
                change(.default(target: target, communications: true))
            }
        }
        if control.canEnable {
            Button("Disable \(name)") { change(.enabled(target: target, enabled: false)) }
        }
    }
}

/// A volume slider (0 to 100%) showing the device's value, except while
/// it is being moved; changes are sent as it moves (at most every 100 ms)
/// and when it is let go.
struct VolumeSlider: View {
    let name: String
    let value: Float
    let send: (Float) -> Void
    @State private var moving: Double?
    @State private var lastSent = Date.distantPast

    var body: some View {
        let shown = moving ?? Double(value)
        Slider(value: Binding(get: { shown }, set: { v in
            moving = v
            if Date().timeIntervalSince(lastSent) >= 0.1 {
                lastSent = Date()
                send(Float(v))
            }
        }), in: 0...1, step: 0.01) { editing in
            if !editing, let v = moving {
                send(Float(v))
                moving = nil
            }
        }
        .accessibilityLabel("Volume of \(name)")
        .accessibilityValue("\(Int((shown * 100).rounded()))%")
        // VoiceOver's adjustments come without an editing end: send each.
        .accessibilityAdjustableAction { direction in
            let step = direction == .increment ? 0.05 : -0.05
            let v = min(1, max(0, Double(value) + step))
            send(Float(v))
        }
        .onChange(of: value) { _, _ in
            if moving == nil { lastSent = .distantPast }
        }
    }
}

/// Whose sound settings are open.
struct SoundSettingsTarget: Identifiable, Hashable {
    let account: String
    let nodeId: String
    var id: String { "\(account)/\(nodeId)" }
}
