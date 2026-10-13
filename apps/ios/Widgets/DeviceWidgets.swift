import AppIntents
import SwiftUI
import WidgetKit

// AudioNet's widgets: the accounts' devices at a glance (Home Screen,
// StandBy, Lock Screen), from the shared state the app and the
// notification extension keep current. Listen buttons run in the app.
// Every state is in words.

struct StateEntry: TimelineEntry {
    let date: Date
    let state: SharedState
}

/// One entry, refreshed when AudioNet reloads widgets (it does whenever a
/// device or stream changes) and hourly for the "as of" time.
struct StateProvider: TimelineProvider {
    func placeholder(in context: Context) -> StateEntry { StateEntry(date: .now, state: .preview) }

    func getSnapshot(in context: Context, completion: @escaping (StateEntry) -> Void) {
        completion(StateEntry(date: .now, state: context.isPreview ? .preview : SharedState.load()))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<StateEntry>) -> Void) {
        completion(Timeline(entries: [StateEntry(date: .now, state: SharedState.load())],
                            policy: .after(.now.addingTimeInterval(3600))))
    }
}

extension SharedState {
    /// What the widget gallery shows.
    static var preview: SharedState {
        var s = SharedState()
        s.signedIn = true
        s.online = true
        s.accountCount = 1
        s.devices = [
            Device(nodeId: "a", name: "Studio PC", account: "x", accountName: "you", online: true, sharing: true, platform: "windows"),
            Device(nodeId: "b", name: "MacBook", account: "x", accountName: "you", online: true, sharing: false, platform: "macos"),
            Device(nodeId: "c", name: "Living Room PC", account: "x", accountName: "you", online: false, sharing: false, platform: "windows"),
        ]
        s.updated = .now
        return s
    }

    /// "Listening to Studio PC", "2 streams", or nil.
    var streamLine: String? {
        guard let first = streams.first else { return nil }
        if streams.count > 1 { return "\(streams.count) streams" }
        return first.listening ? "Listening to \(first.deviceName)" : "Sending to \(first.deviceName)"
    }

    /// "As of 3:12 PM" when AudioNet has had no news for a while.
    var staleLine: String? {
        guard Date().timeIntervalSince(updated) > 30 * 60, updated != .distantPast else { return nil }
        return "As of \(updated.formatted(date: .omitted, time: .shortened))"
    }
}

// MARK: All devices

struct DevicesWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: "AudioNetDevices", provider: StateProvider()) { entry in
            DevicesView(state: entry.state)
                .containerBackground(.fill.tertiary, for: .widget)
                .widgetURL(URL(string: "audionet://open"))
        }
        .configurationDisplayName("AudioNet Devices")
        .description("Your devices: online or offline, and sharing. Listen to one from here.")
        .supportedFamilies([.systemSmall, .systemMedium, .systemLarge,
                            .accessoryInline, .accessoryRectangular, .accessoryCircular])
    }
}

struct DevicesView: View {
    @Environment(\.widgetFamily) private var family
    let state: SharedState

    var body: some View {
        switch family {
        case .accessoryInline:
            Text("AudioNet: \(state.summary)")
        case .accessoryCircular:
            VStack(spacing: 0) {
                Text("\(state.onlineCount)").font(.title2.bold())
                Text("online").font(.caption2)
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("AudioNet: \(state.summary)")
        case .accessoryRectangular:
            VStack(alignment: .leading) {
                Text("AudioNet").font(.headline)
                Text(state.summary)
                if let line = state.streamLine { Text(line) }
            }
        case .systemSmall:
            VStack(alignment: .leading, spacing: 4) {
                Text("AudioNet").font(.headline)
                Text(state.summary).font(.title3.bold())
                if let line = state.streamLine { Text(line).font(.caption) }
                Spacer(minLength: 0)
                if let stale = state.staleLine { Text(stale).font(.caption2) }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        default:
            DeviceList(state: state, limit: family == .systemLarge ? 7 : 3)
        }
    }
}

/// A row per device with its state in words, and Listen where it can be
/// listened to.
struct DeviceList: View {
    let state: SharedState
    let limit: Int

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("AudioNet").font(.headline)
                Spacer()
                Text(state.streamLine ?? state.summary).font(.caption)
            }
            if state.devices.isEmpty {
                Text(state.signedIn ? "No other devices yet." : "Open AudioNet to sign in.")
            }
            ForEach(state.devices.prefix(limit)) { d in
                HStack {
                    Text("\(d.name): \(d.stateText)")
                        .lineLimit(1)
                    Spacer()
                    if state.isListening(to: d.nodeId) {
                        Text("Listening").font(.caption.bold())
                    } else if d.listenable {
                        Button(intent: ListenIntent(device: DeviceEntity(d))) {
                            Text("Listen")
                        }
                        .accessibilityLabel("Listen to \(d.name)")
                    }
                }
            }
            Spacer(minLength: 0)
            if let stale = state.staleLine { Text(stale).font(.caption2) }
        }
    }
}

// MARK: One device

struct DeviceWidget: Widget {
    var body: some WidgetConfiguration {
        AppIntentConfiguration(kind: "AudioNetDevice", intent: SelectDeviceIntent.self,
                               provider: DeviceProvider()) { entry in
            DeviceView(entry: entry)
                .containerBackground(.fill.tertiary, for: .widget)
                .widgetURL(entry.device.map { URL(string: "audionet://device/\($0.nodeId)")! })
        }
        .configurationDisplayName("AudioNet Device")
        .description("One device: online or offline, and sharing. Listen to it from here.")
        .supportedFamilies([.systemSmall, .accessoryInline, .accessoryRectangular])
    }
}

struct DeviceEntry: TimelineEntry {
    let date: Date
    let device: SharedState.Device?
    let listening: Bool
    let chosen: Bool
}

struct DeviceProvider: AppIntentTimelineProvider {
    func placeholder(in context: Context) -> DeviceEntry {
        DeviceEntry(date: .now, device: SharedState.preview.devices.first, listening: false, chosen: true)
    }

    func snapshot(for configuration: SelectDeviceIntent, in context: Context) async -> DeviceEntry {
        context.isPreview ? placeholder(in: context) : entry(configuration)
    }

    func timeline(for configuration: SelectDeviceIntent, in context: Context) async -> Timeline<DeviceEntry> {
        Timeline(entries: [entry(configuration)], policy: .after(.now.addingTimeInterval(3600)))
    }

    private func entry(_ configuration: SelectDeviceIntent) -> DeviceEntry {
        let state = SharedState.load()
        let id = configuration.device?.id
        let device = id.flatMap(state.device)
        return DeviceEntry(date: .now, device: device,
                           listening: id.map(state.isListening(to:)) ?? false,
                           chosen: id != nil)
    }
}

struct DeviceView: View {
    @Environment(\.widgetFamily) private var family
    let entry: DeviceEntry

    var body: some View {
        if let d = entry.device {
            switch family {
            case .accessoryInline:
                Text("\(d.name): \(d.stateText)")
            case .accessoryRectangular:
                VStack(alignment: .leading) {
                    Text(d.name).font(.headline)
                    Text(entry.listening ? "Listening" : d.stateText)
                }
            default:
                VStack(alignment: .leading, spacing: 6) {
                    Text(d.name).font(.headline).lineLimit(2)
                    Text(d.stateText.prefix(1).uppercased() + d.stateText.dropFirst())
                    Spacer(minLength: 0)
                    if entry.listening {
                        Text("Listening").font(.caption.bold())
                    } else if d.listenable {
                        Button(intent: ListenIntent(device: DeviceEntity(d))) {
                            Label("Listen", systemImage: "headphones")
                        }
                        .accessibilityLabel("Listen to \(d.name)")
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        } else {
            Text(entry.chosen ? "This device is no longer in your accounts." : "Choose a device: touch and hold, then Edit Widget.")
                .font(.caption)
        }
    }
}
