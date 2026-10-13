import AppIntents
import Combine
import CoreSpotlight
import Foundation
import SwiftUI
import WidgetKit

/// Keeps what lives outside the app current as AudioNet's state changes:
/// the shared state file (widgets, Control Center, Siri), the Live
/// Activity, the Lock Screen player (when turned on), Siri's list of
/// devices and Spotlight. Changes are gathered for a quarter of a second,
/// and nothing is written or reloaded when nothing changed.
@MainActor
final class Integrations {
    static let shared = Integrations()

    let activity = StreamsActivityController()
    let nowPlaying = NowPlayingController()

    /// Streams on this iPhone, including those other devices started (from
    /// the engine).
    var engineStreams = 0 {
        didSet { if engineStreams != oldValue { scheduleRefresh() } }
    }

    private weak var model: AppModel?
    private var watch: AnyCancellable?
    private var pending: Task<Void, Never>?
    private var lastState: SharedState?

    /// Starts following the model (once, at launch).
    func start(_ model: AppModel) {
        guard self.model == nil else { return }
        self.model = model
        AudioNetActions.handler = model
        activity.endLeftovers()
        nowPlaying.model = model
        watch = model.objectWillChange.sink { [weak self] _ in self?.scheduleRefresh() }
        scheduleRefresh()
    }

    private func scheduleRefresh() {
        pending?.cancel()
        pending = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled else { return }
            self?.refresh()
        }
    }

    /// Brings everything outside the app up to date.
    func refresh() {
        guard let model else { return }
        let others = max(0, engineStreams - model.streams.count)
        activity.update(streams: model.streams, others: others)
        nowPlaying.update(streams: model.streams)

        var state = SharedState.load()
        let before = state
        state.signedIn = model.signedIn
        state.online = model.online
        state.accountCount = model.accounts.count
        state.devices = devices(model, keeping: before)
        state.streams = model.streams.map {
            SharedState.Stream(id: $0.id, title: $0.title, state: $0.state, muted: $0.muted,
                               nodeId: $0.nodeId, deviceName: $0.deviceName, listening: $0.listening)
        }
        state.otherStreams = others
        state.updated = before.updated
        // Nothing new since the last write (by the app, or a notification).
        if state == before && lastState != nil { return }
        state.updated = Date()
        state.save()
        let devicesChanged = lastState?.devices != state.devices
        lastState = state
        WidgetCenter.shared.reloadAllTimelines()
        if #available(iOS 18.0, *) {
            ControlCenter.shared.reloadAllControls()
        }
        if devicesChanged {
            AudioNetShortcuts.updateAppShortcutParameters()
            index(state.devices)
        }
    }

    /// The devices to share: what each connected account lists now, and for
    /// an account still connecting (or reconnecting), what it listed last.
    private func devices(_ model: AppModel, keeping before: SharedState) -> [SharedState.Device] {
        model.accounts.flatMap { a -> [SharedState.Device] in
            guard model.devicesLoaded.contains(a.nodeId) else {
                return before.devices.filter { $0.account == a.nodeId }
            }
            return (model.devicesByAccount[a.nodeId] ?? []).map { d in
                SharedState.Device(nodeId: d.nodeId, name: d.name, account: a.nodeId,
                                   accountName: a.accountName, online: d.online, sharing: d.sharing,
                                   platform: d.platform)
            }
        }
    }

    /// Devices in Spotlight (iOS 18 and later): finding "Studio PC" opens it
    /// in AudioNet.
    private func index(_ devices: [SharedState.Device]) {
        guard #available(iOS 18.0, *) else { return }
        let entities = devices.map(DeviceEntity.init)
        Task.detached {
            let index = CSSearchableIndex.default()
            try? await index.deleteAppEntities(ofType: DeviceEntity.self)
            try? await index.indexAppEntities(entities)
        }
    }

    /// A stream ended (the model has already removed it).
    func streamEnded(_ row: StreamRow, reason: String, byUser: Bool, remaining: Int) {
        activity.streamEnded(row, reason: reason, byUser: byUser, remaining: remaining)
        scheduleRefresh()
    }
}

// MARK: What Siri, widgets, controls and the Live Activity's buttons do

extension AppModel: AudioNetActionsHandler {
    /// A device by id, once this iPhone is connected and its account has
    /// listed it (AudioNet may have just been started for this, in the
    /// background): waits up to 15 seconds.
    func waitForDevice(_ nodeId: String) async throws -> Device {
        for _ in 0..<60 {
            if !readingAccount && !signedIn {
                throw AudioNetIntentError("AudioNet is not signed in on this iPhone. Open AudioNet to sign in.")
            }
            if let d = devices.first(where: { $0.nodeId == nodeId }) { return d }
            let loaded = !accounts.isEmpty && accounts.allSatisfy { devicesLoaded.contains($0.nodeId) }
            if loaded { break }
            try await Task.sleep(for: .milliseconds(250))
        }
        if let d = devices.first(where: { $0.nodeId == nodeId }) { return d }
        let name = SharedState.load().device(nodeId)?.name ?? "That device"
        throw AudioNetIntentError(online
            ? "\(name) is no longer in your AudioNet accounts."
            : "AudioNet could not reach your accounts. Check the connection and try again.")
    }

    func listen(to nodeId: String) async throws -> String {
        if let row = streams.first(where: { $0.listening && $0.nodeId == nodeId }) {
            return "Already listening to \(row.deviceName)."
        }
        let d = try await waitForDevice(nodeId)
        let title = try startListening(d)
        return "Starting: \(title)."
    }

    func stopListening(to nodeId: String) -> String {
        let rows = streams.filter { $0.listening && $0.nodeId == nodeId }
        guard let first = rows.first else { return "Not listening to that device." }
        rows.forEach(stopStream)
        return "Stopping: listening to \(first.deviceName)."
    }

    func sendMicrophone(to nodeId: String) async throws -> String {
        let d = try await waitForDevice(nodeId)
        let title = try startSending(d)
        return "Starting: \(title)."
    }

    func stopAll() -> String {
        let count = streams.count
        guard count > 0 else { return "Nothing is streaming." }
        streams.forEach(stopStream)
        return count == 1 ? "Stopping the stream." : "Stopping \(count) streams."
    }

    func setMuted(_ muted: Bool) -> String {
        guard !streams.isEmpty else { return "Nothing is streaming." }
        for row in streams { updateVolume(row.id) { $0.muted = muted } }
        let text = muted ? "AudioNet is muted." : "AudioNet is unmuted."
        announce(text)
        return text
    }

    func open(device nodeId: String) {
        guard let d = devices.first(where: { $0.nodeId == nodeId }) else {
            if signedIn { announce("That device is not in your accounts now.") }
            return
        }
        expanded.insert(nodeId)
        announce("\(d.name): \(deviceState(d)).")
    }

    // MARK: Links and Handoff

    /// audionet://device/ID opens a device; audionet://listen/ID listens to
    /// it (widgets, Shortcuts, NFC tags).
    func openLink(_ url: URL) {
        guard url.scheme == "audionet", let nodeId = url.pathComponents.dropFirst().first else { return }
        switch url.host {
        case "listen":
            Task {
                do { _ = try await listen(to: nodeId) } catch {
                    announce((error as? AudioNetIntentError)?.message ?? describe(error))
                }
            }
        default:
            Task {
                _ = try? await waitForDevice(nodeId)
                open(device: nodeId)
            }
        }
    }

    /// The stream offered to the Mac through Handoff: the first one listened
    /// to.
    var handoffStream: StreamRow? { streams.first { $0.listening } }

    /// Describes a stream being listened to, for Handoff.
    func describeHandoff(_ activity: NSUserActivity) {
        guard let row = handoffStream,
              let account = accounts.first(where: { $0.nodeId == row.account }) else { return }
        activity.title = "Listening to \(row.deviceName)"
        activity.isEligibleForHandoff = true
        activity.addUserInfoEntries(from: [
            "server": account.serverUrl,
            "username": account.username,
            "node_id": row.nodeId,
            "source_id": row.sourceId,
        ])
    }

    /// Listens here to what the Mac was listening to.
    func continueListening(_ activity: NSUserActivity) {
        guard let info = activity.userInfo,
              let server = info["server"] as? String,
              let nodeId = info["node_id"] as? String else { return }
        let sourceId = info["source_id"] as? String
        let wanted = Account(serverUrl: server, nodeId: "", token: "", deviceName: "",
                             username: info["username"] as? String ?? "")
        guard accounts.contains(where: { $0.sameAccount(as: wanted) }) else {
            announce("This iPhone is not signed in to the account the Mac was listening in.")
            return
        }
        Task {
            do {
                let d = try await waitForDevice(nodeId)
                if let sourceId, d.sources.contains(where: { $0.id == sourceId }) {
                    choices[nodeId, default: DeviceChoices()].listenSource = sourceId
                }
                _ = try startListening(d)
            } catch {
                announce((error as? AudioNetIntentError)?.message ?? describe(error))
            }
        }
    }
}
