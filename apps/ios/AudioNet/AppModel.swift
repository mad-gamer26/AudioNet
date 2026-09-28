import AVFoundation
import Foundation
import SwiftUI
import UIKit

/// Hands engine events (which arrive on an engine thread) to the main actor.
final class EngineEvents: EventListener, @unchecked Sendable {
    private let handler: @Sendable (Event) -> Void
    init(_ handler: @escaping @Sendable (Event) -> Void) { self.handler = handler }
    func onEvent(event: Event) { handler(event) }
}

/// A stream this iPhone started.
struct StreamRow: Identifiable, Equatable {
    let id: String
    /// The account (this iPhone's device id in it) the stream runs in.
    let account: String
    let title: String
    var state: String
    /// Volume slider position (0 to 1) and mute, on this device: what the
    /// stream plays here, or what it sends from here.
    var volume = 1.0
    var muted = false
}

/// Everything the screens show, and every action they take.
///
/// The iPhone can be signed in to several AudioNet accounts; it is a
/// separate device in each, with its own connection (engine client). Going
/// online or offline applies to all of them.
@MainActor
final class AppModel: ObservableObject {
    /// The accounts this iPhone is signed in to (each with this iPhone's
    /// device id in it as `nodeId`).
    @Published private(set) var accounts: [Account] = []
    @Published private(set) var readingAccount = false
    /// Connected to its accounts: while AudioNet runs and is signed in,
    /// this iPhone is online in every account (sharing or not).
    @Published private(set) var online = false
    /// Accounts in which this iPhone shares its audio (by its device id
    /// there): others may listen to it and it may send its own. Not
    /// sharing, it still sees the devices, listens to them and plays what
    /// they send it. Each account's choice is saved.
    @Published private(set) var sharingAccounts: Set<String> = []
    /// Accounts being signed out of (their device is being removed).
    @Published private(set) var signingOut: Set<String> = []
    /// Accounts whose connection is up (by this iPhone's device id there).
    @Published private(set) var connectedAccounts: Set<String> = []
    @Published private(set) var signingIn = false
    /// Why signing in cannot go on, shown on the sign-in form (and spoken).
    @Published private(set) var signInProblem: String?
    /// Each account's devices (by this iPhone's device id there).
    @Published private(set) var devicesByAccount: [String: [Device]] = [:]
    /// Accounts whose device list the server has sent since going online.
    @Published private(set) var devicesLoaded: Set<String> = []
    @Published private(set) var local = LocalAudio(sources: [], outputs: [])
    @Published private(set) var streams: [StreamRow] = []
    @Published private(set) var log: [String] = []
    /// Each stream's measurements (a line of its own: the updates every two
    /// seconds redraw only that line). The list changes only as streams
    /// start and end.
    @Published private(set) var diagnostics: [String: LiveText] = [:]

    /// What to listen to and send, per device (node id).
    struct DeviceChoices {
        var listenSource: String?
        var listenOutput: String?
        var sendSource: String?
        var sendOutput: String?
    }
    @Published var choices: [String: DeviceChoices] = [:]
    /// Devices whose disclosure is open (node ids).
    @Published var expanded: Set<String> = []

    /// 0.7's "go online when AudioNet opens" (one switch for all accounts):
    /// read once, to carry that choice over to accounts with no sharing
    /// choice of their own yet. No longer offered.
    @AppStorage("startSharing", store: AccountStore.settings) var startSharing = false
    /// The microphones and the one chosen (a separate object: the list
    /// screen redraws only when they change, never for log or stream
    /// updates).
    let microphones = MicrophoneStore()

    /// Each account's connection (by this iPhone's device id there).
    private var clients: [String: Client] = [:]
    /// Counts engine starts per account: events from an engine that was
    /// stopped (they can still be on their way) are ignored.
    private var generations: [String: Int] = [:]
    private var micWatch: NSObjectProtocol?

    /// Signed in to at least one account.
    var signedIn: Bool { !accounts.isEmpty }

    /// Every account connected.
    var connected: Bool { online && !accounts.isEmpty && accounts.allSatisfy { connectedAccounts.contains($0.nodeId) } }

    /// The devices of every account, in account order.
    var devices: [Device] { accounts.flatMap { devicesByAccount[$0.nodeId] ?? [] } }

    init() {
        AccountStore.prepareTestProfile()
        microphones.start()
        micWatch = NotificationCenter.default.addObserver(
            forName: AudioSession.microphoneInUse, object: nil, queue: .main) { note in
            guard let text = note.object as? String else { return }
            MainActor.assumeIsolated { self.note(text) }
        }
        guard AccountStore.hasStoredAccount else {
            note("To add this iPhone, sign in with your AudioNet account name and password.")
            return
        }
        readingAccount = true
        let goOnlineNow = startSharing
        Task.detached {
            let (accounts, problem) = AccountStore.loadAll()
            await MainActor.run { self.accountsRead(accounts, problem, goOnline: goOnlineNow) }
        }
    }

    private func accountsRead(_ read: [Account], _ problem: String?, goOnline: Bool) {
        readingAccount = false
        accounts = read
        if let problem { announce(problem) }
        if read.isEmpty {
            if problem == nil { note("To add this iPhone, sign in with your AudioNet account name and password.") }
            return
        }
        note("Welcome back. This iPhone is signed in to \(accountList).")
        // An account with no saved choice yet (0.7 had one switch for all)
        // shares if 0.7 went online at launch.
        for a in read where savedSharing(a.nodeId) == nil {
            AccountStore.settings.set(goOnline, forKey: "sharing-\(a.nodeId)")
        }
        connectAll()
    }

    /// The accounts in words: "mad-gamer26 on audionet.example.com and ...".
    var accountList: String {
        ListFormatter.localizedString(byJoining: accounts.map(\.accountName))
    }

    var defaultServer: String {
        accounts.first?.serverUrl
            ?? (Bundle.main.object(forInfoDictionaryKey: "AudioNetDefaultServer") as? String) ?? ""
    }

    func choice(_ nodeId: String, _ key: WritableKeyPath<DeviceChoices, String?>) -> Binding<String?> {
        Binding(get: { self.choices[nodeId]?[keyPath: key] },
                set: { self.choices[nodeId, default: DeviceChoices()][keyPath: key] = $0 })
    }

    func expansion(_ nodeId: String) -> Binding<Bool> {
        Binding(get: { self.expanded.contains(nodeId) },
                set: { open in
                    if open { self.expanded.insert(nodeId) } else { self.expanded.remove(nodeId) }
                })
    }

    // MARK: Log and announcements

    func note(_ text: String) {
        log.append(text)
        if log.count > 300 { log.removeFirst(log.count - 300) }
    }

    /// A sign-in problem: on the form, in the log, and spoken.
    func reportSignInProblem(_ text: String) {
        signInProblem = text
        announce(text)
    }

    /// Writes to the log and has VoiceOver say it once.
    func announce(_ text: String) {
        note(text)
        AccessibilityNotification.Announcement(text).post()
    }

    // MARK: Accounts

    /// Adds this iPhone to an account (the first, or another one). `done`
    /// is called with true once it is signed in.
    func signIn(server: String, username: String, password: String, deviceName: String,
                done: @escaping @MainActor (Bool) -> Void = { _ in }) {
        guard !signingIn else { return }
        let wanted = Account(serverUrl: server, nodeId: "", token: "", deviceName: deviceName, username: username)
        if let same = accounts.first(where: { $0.sameAccount(as: wanted) }) {
            reportSignInProblem("This iPhone is already signed in to \(same.accountName).")
            return
        }
        signingIn = true
        signInProblem = nil
        announce("Signing in.")
        Task {
            // Blocking network call: off the main thread.
            let result = await Task.detached {
                Result { try AudioNet.signIn(serverUrl: server, username: username,
                                             password: password, deviceName: deviceName) }
            }.value
            do {
                self.signingIn = false
                switch result {
                case .success(let account):
                    self.accounts.append(account)
                    if let problem = AccountStore.saveAll(self.accounts) { self.announce(problem) }
                    self.announce("Signed in as \"\(account.deviceName)\" to \(account.accountName). This iPhone is online there, not sharing its audio: turn on sharing to share it.")
                    self.connectAll()
                    self.connect(account)
                    done(true)
                case .failure(let error):
                    self.signInProblem = "Signing in failed: \(describe(error))"
                    self.announce(self.signInProblem ?? "")
                    done(false)
                }
            }
        }
    }

    /// Signs this iPhone out of one account, sharing or not: its connection
    /// stops, then the server removes this iPhone's device from the account
    /// (nothing is left behind). If the server cannot be reached, it says
    /// so and stays signed in there.
    func signOut(_ account: Account) {
        let id = account.nodeId
        guard !signingOut.contains(id) else { return }
        signingOut.insert(id)
        disconnect(id)
        announce("Signing out of \(account.accountName).")
        Task {
            let result = await Task.detached { Result { try removeDevice(account: account) } }.value
            self.signingOut.remove(id)
            switch result {
            case .success:
                self.accounts.removeAll { $0.nodeId == id }
                self.sharingAccounts.remove(id)
                AccountStore.settings.removeObject(forKey: "sharing-\(id)")
                if let problem = AccountStore.saveAll(self.accounts) { self.announce(problem) }
                if self.accounts.isEmpty {
                    self.disconnectAll()
                    AccountStore.clear()
                }
                self.announce("Signed out of \(account.accountName): this iPhone was removed from that account.")
            case .failure(let error):
                self.connect(account)
                self.announce("Could not sign out of \(account.accountName): \(describe(error)) This iPhone is still signed in there.")
            }
        }
    }

    // MARK: Microphone

    /// Chooses the microphone to record from; streams recording now switch
    /// to it at once.
    func chooseMicrophone(_ m: Microphone) {
        microphones.choose(m)
        announce("Microphone: \(m.fullName).")
    }

    // MARK: Connections

    /// Connects every account: while AudioNet runs, this iPhone is online in
    /// all of them.
    func connectAll() {
        guard !accounts.isEmpty, !online else { return }
        // Mixing with other audio, never interrupting it; playback only
        // until the microphone is recorded. The engine reports its streams
        // (including those other devices start here), across all accounts.
        setAudioUseListener(listener: AudioUse())
        AudioSession.goOnline()
        online = true
        accounts.forEach(connect)
        refreshLocalAudio()
        Task { await requestMicrophoneIfNeeded() }
    }

    /// Starts one account's connection.
    private func connect(_ account: Account) {
        let id = account.nodeId
        guard clients[id] == nil else { return }
        let generation = (generations[id] ?? 0) + 1
        generations[id] = generation
        let events = EngineEvents { event in
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard self.generations[id] == generation else { return }
                    self.handle(event, account: id)
                }
            }
        }
        let c = Client(account: account, listener: events)
        let share = savedSharing(id) ?? false
        c.setSharing(sharing: share)
        if share { sharingAccounts.insert(id) } else { sharingAccounts.remove(id) }
        clients[id] = c
        c.start()
    }

    /// Stops one account's connection and forgets what it showed.
    private func disconnect(_ id: String) {
        generations[id, default: 0] += 1
        connectedAccounts.remove(id)
        devicesByAccount[id] = nil
        devicesLoaded.remove(id)
        let ended = streams.filter { $0.account == id }
        streams.removeAll { $0.account == id }
        for row in ended { diagnostics[row.id] = nil }
        if let c = clients.removeValue(forKey: id) {
            Task.detached { c.stop() }
        }
    }

    /// Disconnects every account (after signing out of the last one).
    private func disconnectAll() {
        guard online else { return }
        for id in Array(clients.keys) { disconnect(id) }
        clearOnlineState()
        Task.detached {
            // After the engines have closed their streams.
            try? await Task.sleep(for: .milliseconds(300))
            await MainActor.run { AudioSession.goOffline() }
        }
    }

    /// Offline: nothing the servers told us is shown any more (devices, their
    /// open disclosures, streams and measurements).
    private func clearOnlineState() {
        online = false
        sharingAccounts = []
        connectedAccounts = []
        streams = []
        devicesByAccount = [:]
        devicesLoaded = []
        expanded = []
        diagnostics = [:]
    }

    private func requestMicrophoneIfNeeded() async {
        switch AVAudioApplication.shared.recordPermission {
        case .undetermined:
            let granted = await AVAudioApplication.requestRecordPermission()
            if granted { refreshLocalAudio() } else { microphoneBlocked() }
        case .denied:
            microphoneBlocked()
        default:
            break
        }
    }

    private func microphoneBlocked() {
        note("Microphone access is off, so this iPhone's microphone cannot be sent. To allow it: Settings, Apps, AudioNet, Microphone.")
    }

    func refreshLocalAudio() {
        Task.detached {
            let audio = (try? AudioNet.localAudio()) ?? LocalAudio(sources: [], outputs: [])
            await MainActor.run {
                self.local = audio
                self.devices.forEach(self.fillChoices)
            }
        }
    }

    // MARK: Remote

    /// The connection to a device's account, or nil (and says why).
    private func client(for d: Device) -> (account: String, client: Client)? {
        guard let account = account(of: d) else {
            announce("\(d.name) is no longer listed in your accounts.")
            return nil
        }
        guard let c = clients[account] else {
            announce("Not connected to \(name(account)) yet. Try again in a moment.")
            return nil
        }
        return (account, c)
    }

    /// The account (this iPhone's device id there) a device belongs to.
    func account(of d: Device) -> String? {
        devicesByAccount.first { $0.value.contains { $0.nodeId == d.nodeId } }?.key
    }

    /// Keeps a device's choices valid: its default sound and output, and
    /// this iPhone's output and microphone, where nothing is chosen yet.
    func fillChoices(_ d: Device) {
        var c = choices[d.nodeId] ?? DeviceChoices()
        if !d.sources.contains(where: { $0.id == c.listenSource }) {
            c.listenSource = (d.sources.first { $0.isDefault } ?? d.sources.first)?.id
        }
        if !d.outputs.contains(where: { $0.id == c.sendOutput }) {
            c.sendOutput = (d.outputs.first { $0.isDefault } ?? d.outputs.first)?.id
        }
        if !local.outputs.contains(where: { $0.id == c.listenOutput }) {
            c.listenOutput = (local.outputs.first { $0.isDefault } ?? local.outputs.first)?.id
        }
        if !local.sources.contains(where: { $0.id == c.sendSource }) {
            c.sendSource = (local.sources.first { $0.isDefault && $0.isInput } ?? local.sources.first)?.id
        }
        choices[d.nodeId] = c
    }

    func listen(_ d: Device) {
        guard let (account, c) = client(for: d), usable(d), listenable(d) else { return }
        let ch = choices[d.nodeId] ?? DeviceChoices()
        guard let s = d.sources.first(where: { $0.id == ch.listenSource }),
              let o = local.outputs.first(where: { $0.id == ch.listenOutput }) else {
            announce("Choose the sound to listen to.")
            return
        }
        start(account: account, title: "Listening to \(s.name) on \(d.name), playing on \(AudioSession.currentOutput)") {
            try c.listen(nodeId: d.nodeId, sourceId: s.id, outputId: o.id)
        }
    }

    func send(_ d: Device) {
        guard let (account, c) = client(for: d), usable(d) else { return }
        guard isSharing(account) else {
            announce("This iPhone is not sharing its audio in \(name(account)). Turn on sharing there to send from it.")
            return
        }
        let ch = choices[d.nodeId] ?? DeviceChoices()
        guard let s = local.sources.first(where: { $0.id == ch.sendSource }),
              let o = d.outputs.first(where: { $0.id == ch.sendOutput }) else {
            announce(local.sources.isEmpty
                     ? "This iPhone's microphone is not available. Allow microphone access for AudioNet in Settings."
                     : "Choose where it should play.")
            return
        }
        start(account: account, title: "Sending this iPhone's \(s.name.lowercased()) to \(o.name) on \(d.name)") {
            try c.send(nodeId: d.nodeId, sourceId: s.id, outputId: o.id)
        }
    }

    // MARK: Sharing

    /// The saved choice for an account, if it was ever made.
    private func savedSharing(_ id: String) -> Bool? {
        AccountStore.settings.object(forKey: "sharing-\(id)") as? Bool
    }

    /// Whether this iPhone shares its audio in that account.
    func isSharing(_ id: String) -> Bool { sharingAccounts.contains(id) }

    /// A switch for one account's sharing.
    func sharing(_ id: String) -> Binding<Bool> {
        Binding(get: { self.sharingAccounts.contains(id) },
                set: { self.setSharing(id, $0) })
    }

    /// Starts or stops sharing this iPhone's audio in one account, and saves
    /// the choice. Stopping ends every stream it sends there; what it
    /// receives goes on.
    func setSharing(_ id: String, _ on: Bool) {
        guard accounts.contains(where: { $0.nodeId == id }) else { return }
        AccountStore.settings.set(on, forKey: "sharing-\(id)")
        if on { sharingAccounts.insert(id) } else { sharingAccounts.remove(id) }
        clients[id]?.setSharing(sharing: on)
        announce(on
            ? "Sharing this iPhone's audio in \(name(id)): your devices there can listen to it, and it can send its audio."
            : "Stopped sharing in \(name(id)). This iPhone is still online there: it receives audio, but sends none of its own.")
    }

    /// Start or stop sharing everywhere (the menu bar's choice).
    func toggleSharingEverywhere() {
        let on = sharingAccounts.isEmpty
        for a in accounts { setSharing(a.nodeId, on) }
    }

    func stopStream(_ row: StreamRow) {
        clients[row.account]?.stopStream(sessionId: row.id)
    }

    /// A stream's volume slider (0 to 1), applied at once.
    func volume(_ id: String) -> Binding<Double> {
        Binding(get: { self.streams.first { $0.id == id }?.volume ?? 1 },
                set: { v in self.updateVolume(id) { $0.volume = v } })
    }

    /// A stream's mute switch, applied at once.
    func muted(_ id: String) -> Binding<Bool> {
        Binding(get: { self.streams.first { $0.id == id }?.muted ?? false },
                set: { m in self.updateVolume(id) { $0.muted = m } })
    }

    private func updateVolume(_ id: String, _ change: (inout StreamRow) -> Void) {
        guard let i = streams.firstIndex(where: { $0.id == id }) else { return }
        change(&streams[i])
        clients[streams[i].account]?.setStreamVolume(sessionId: id, volume: Float(streams[i].volume),
                                                     muted: streams[i].muted)
    }

    private func usable(_ d: Device) -> Bool {
        guard d.online else {
            announce("\(d.name) is offline.")
            return false
        }
        return true
    }

    /// A device that does not share can be sent to, not listened to.
    private func listenable(_ d: Device) -> Bool {
        guard d.sharing else {
            announce("\(d.name) is not sharing its audio, so it cannot be listened to. You can still send to it.")
            return false
        }
        return true
    }

    private func start(account: String, title: String, _ begin: () throws -> String) {
        do {
            let id = try begin()
            streams.append(StreamRow(id: id, account: account, title: title, state: "starting"))
            announce("Starting: \(title).")
        } catch {
            announce("Could not start: \(describe(error))")
        }
    }

    // MARK: Engine events

    /// The account's name for messages ("mad-gamer26 on audionet.example.com").
    private func name(_ account: String) -> String {
        accounts.first { $0.nodeId == account }?.accountName ?? "an account"
    }

    func handle(_ event: Event, account: String) {
        switch event {
        case .connected:
            connectedAccounts.insert(account)
            let how = isSharing(account) ? "sharing its audio" : "not sharing its audio"
            announce(accounts.count > 1
                ? "Online in \(name(account)), \(how)."
                : "Online, \(how).")
            refreshLocalAudio()
        case .disconnected(let reason):
            connectedAccounts.remove(account)
            note("Disconnected from \(name(account)): \(reason). Reconnecting.")
        case .status(let text):
            note(text)
        case .devices(let list):
            devicesByAccount[account] = list
            devicesLoaded.insert(account)
            list.forEach(fillChoices)
            expanded.formIntersection(devices.map(\.nodeId))
        case .deviceChanged(let device):
            var list = devicesByAccount[account] ?? []
            if let i = list.firstIndex(where: { $0.nodeId == device.nodeId }) {
                let was = list[i]
                list[i] = device
                if was.online != device.online {
                    announce("\(device.name) is now \(device.online ? "online" : "offline").")
                } else if device.online && was.sharing != device.sharing {
                    announce("\(device.name) \(device.sharing ? "started" : "stopped") sharing its audio.")
                }
            } else {
                list.append(device)
            }
            devicesByAccount[account] = list
            fillChoices(device)
        case .stream(let id, let state, let detail):
            guard let i = streams.firstIndex(where: { $0.id == id }) else { return }
            let text = state == .active ? "connected" : detail.trimmingCharacters(in: CharacterSet(charactersIn: ".")).lowercased()
            let first = state == .active && streams[i].state != "connected"
            streams[i].state = text
            if first { announce("\(streams[i].title): connected.") }
            if detail.hasPrefix("Warning:") { announce("\(streams[i].title): \(detail)") }
        case .streamEnded(let id, let reason):
            if let i = streams.firstIndex(where: { $0.id == id }) {
                let row = streams.remove(at: i)
                diagnostics[id] = nil
                announce("Stopped: \(row.title). \(reason)")
            }
        case .diagnostics(let id, let text):
            guard streams.contains(where: { $0.id == id }) else { return }
            if let live = diagnostics[id] {
                if live.text != text { live.text = text }
            } else {
                diagnostics[id] = LiveText(text)
            }
        case .serverProblem(let message):
            announce(message)
        case .stopped(let error):
            clients[account] = nil
            disconnect(account)
            if let error { announce("AudioNet stopped in \(name(account)): \(error)") }
            if clients.isEmpty && online {
                clearOnlineState()
                AudioSession.goOffline()
            }
        }
    }
}

/// An error in words.
func describe(_ error: Error) -> String {
    if case let AudioNetError.Failed(message) = error { return message }
    return error.localizedDescription
}

/// The iPhone's microphones, grouped by input as UniMic lists them, and the
/// one chosen. Read again when a headset comes or goes, and when the list
/// is opened, but published only when something changed, so VoiceOver's
/// place in the list is never lost to a redraw.
@MainActor
final class MicrophoneStore: ObservableObject {
    @Published private(set) var groups: [MicrophoneGroup] = []
    /// The microphone the iPhone uses now (for the check mark while nothing
    /// is chosen).
    @Published private(set) var current: String?

    @AppStorage("microphone", store: AccountStore.settings) private var chosenID = ""
    @AppStorage("microphoneName", store: AccountStore.settings) private var chosenName = ""

    private var routeWatch: NSObjectProtocol?
    private var pending: Task<Void, Never>?
    /// Reading the list changes the session's category, which posts route
    /// notices of its own: those are not headsets coming or going.
    private var quietUntil = Date.distantPast

    /// The chosen microphone, or nil (the one the iPhone uses).
    var chosen: Microphone? {
        chosenID.isEmpty ? nil
            : Microphone(id: chosenID, name: "", fullName: chosenName, stereo: false)
    }

    /// The id shown as selected: the chosen one, else the one in use.
    var selectedID: String? { chosenID.isEmpty ? current : chosenID }

    /// What the Microphone row says.
    var selectedName: String {
        if let id = selectedID, let m = groups.flatMap(\.microphones).first(where: { $0.id == id }) {
            return m.fullName + (m.stereo ? ", stereo" : "")
        }
        return chosenID.isEmpty ? "iPhone default" : chosenName
    }

    func start() {
        let m = chosen
        Task.detached { AudioSession.setMicrophone(m) }
        refresh()
        routeWatch = NotificationCenter.default.addObserver(
            forName: AVAudioSession.routeChangeNotification, object: nil, queue: .main) { note in
            let reason = (note.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt)
                .flatMap(AVAudioSession.RouteChangeReason.init(rawValue:))
            guard reason == .newDeviceAvailable || reason == .oldDeviceUnavailable else { return }
            MainActor.assumeIsolated {
                guard Date() >= self.quietUntil else { return }
                self.refresh()
            }
        }
    }

    /// Chooses the microphone; while recording it takes over at once.
    func choose(_ m: Microphone) {
        chosenID = m.id
        chosenName = m.fullName
        objectWillChange.send()
        let now = chosen
        Task.detached { AudioSession.setMicrophone(now) }
    }

    /// Reads the list again, once for a burst of requests, off the main
    /// thread (not while a stream plays: the last list stays).
    func refresh() {
        pending?.cancel()
        pending = Task {
            try? await Task.sleep(for: .milliseconds(300))
            guard !Task.isCancelled else { return }
            quietUntil = Date().addingTimeInterval(2)
            let found = await Task.detached { AudioSession.microphones() }.value
            quietUntil = Date().addingTimeInterval(1)
            guard let found else { return }
            var groups = found.groups
            // The chosen one stays listed while unplugged, so the choice is
            // kept and named.
            if !chosenID.isEmpty, !groups.flatMap(\.microphones).contains(where: { $0.id == chosenID }) {
                let gone = Microphone(id: chosenID, name: "Not connected", fullName: chosenName,
                                      stereo: false)
                groups.append(MicrophoneGroup(id: "unplugged", name: chosenName, microphones: [gone]))
            }
            if groups != self.groups { self.groups = groups }
            if found.current != current { current = found.current }
        }
    }
}
