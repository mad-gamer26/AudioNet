import Foundation
import Security

/// What AudioNet's widgets, Control Center controls, Live Activity and Siri
/// know about the accounts' devices and this iPhone's streams, shared by the
/// app and its extensions (in their shared keychain group).
///
/// The app writes it whenever something changes; the notification extension
/// updates a device's state when a "Studio PC is online" notification
/// arrives, so widgets stay current while AudioNet is not running. Device
/// names and states only: never a sign-in token or a password.
struct SharedState: Codable, Equatable, Sendable {
    /// One of the accounts' devices (not this iPhone).
    struct Device: Codable, Equatable, Sendable, Identifiable {
        var id: String { nodeId }
        var nodeId: String
        var name: String
        /// This iPhone's device id in the device's account.
        var account: String
        /// The account in words ("alice on audionet.example.com").
        var accountName: String
        var online: Bool
        var sharing: Bool
        var platform: String

        /// Its state in words: "online", "online, not sharing", "offline".
        var stateText: String {
            !online ? "offline" : sharing ? "online" : "online, not sharing"
        }

        /// It can be listened to now.
        var listenable: Bool { online && sharing }
    }

    /// A stream this iPhone started.
    struct Stream: Codable, Equatable, Sendable, Identifiable {
        var id: String
        /// "Listening to Sound playing on Speakers on Studio PC, playing on
        /// AirPods" and the like.
        var title: String
        /// "starting", "connected", "reconnecting" and the like.
        var state: String
        var muted: Bool
        /// The other device.
        var nodeId: String
        var deviceName: String
        /// Listening to the other device (else sending to it).
        var listening: Bool
    }

    var signedIn = false
    var online = false
    var accountCount = 0
    var devices: [Device] = []
    var streams: [Stream] = []
    /// Streams other devices started here (listening to this iPhone, or
    /// playing on it).
    var otherStreams = 0
    /// When the app last wrote this.
    var updated = Date.distantPast

    // Settings the extensions need (set in the app, and by Focus).

    /// "Offline alerts break through Focus": offline notifications are
    /// Time Sensitive.
    var timeSensitiveOffline = false
    /// A Focus filter asked for AudioNet's notifications to arrive quietly
    /// (in the notification list, without sound or lighting the screen).
    var quietNotifications = false

    var onlineCount: Int { devices.filter(\.online).count }

    /// "3 of 4 devices online", or why there are none.
    var summary: String {
        if !signedIn { return "Not signed in" }
        if devices.isEmpty { return "No other devices" }
        return "\(onlineCount) of \(devices.count) devices online"
    }

    /// Every stream started here is muted (and there is one).
    var allMuted: Bool { !streams.isEmpty && streams.allSatisfy(\.muted) }

    /// Listening to this device now.
    func isListening(to nodeId: String) -> Bool {
        streams.contains { $0.listening && $0.nodeId == nodeId }
    }

    func device(_ nodeId: String) -> Device? {
        devices.first { $0.nodeId == nodeId }
    }

    // MARK: Where it is kept

    /// In the keychain group the app and its extensions share (the one the
    /// notification key is in): readable after the iPhone's first unlock,
    /// when widgets and notifications need it, and never backed up or
    /// moved to another device. A keychain group needs no registration
    /// with Apple, unlike an app group.
    private static var query: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "AudioNet shared state",
            kSecAttrAccount as String: "state",
            kSecAttrAccessGroup as String: Bundle.main.object(forInfoDictionaryKey: "AudioNetKeychainGroup") as? String ?? "",
        ]
    }

    /// The last state written, or an empty one.
    static func load() -> SharedState {
        var read = query
        read[kSecReturnData as String] = true
        var found: CFTypeRef?
        guard SecItemCopyMatching(read as CFDictionary, &found) == errSecSuccess,
              let data = found as? Data,
              let state = try? JSONDecoder().decode(SharedState.self, from: data) else {
            return SharedState()
        }
        return state
    }

    /// Writes this state.
    func save() {
        guard let data = try? JSONEncoder().encode(self) else { return }
        let q = Self.query
        let status = SecItemUpdate(q as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecItemNotFound {
            var add = q
            add[kSecValueData as String] = data
            add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            SecItemAdd(add as CFDictionary, nil)
        }
    }

    /// Changes the stored state in place (the notification extension's
    /// updates, the Focus filter).
    static func update(_ change: (inout SharedState) -> Void) {
        var state = load()
        change(&state)
        state.save()
    }
}
