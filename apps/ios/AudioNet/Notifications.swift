import SwiftUI
import UIKit
import UserNotifications

/// Notifications about the other devices in this iPhone's accounts: coming
/// online or going offline, and starting or stopping sharing their audio.
/// Off until turned on in Settings. Each server sends them through the push
/// gateway it names (`push_gateway` in its info); the text is encrypted
/// with this iPhone's own key and decrypted by the notification extension
/// (NotificationService), so neither the gateway nor Apple can read it.
/// Nothing is shown while AudioNet is open (see `AppDelegate`).
@MainActor
final class Notifications: ObservableObject {
    static let shared = Notifications()
    /// Shared with the extension through a keychain group: the key
    /// notifications are sealed with.
    static let keyName = "notificationKey"

    @AppStorage("notifyPresence", store: AccountStore.settings) var presence = false {
        didSet { changed() }
    }
    @AppStorage("notifySharing", store: AccountStore.settings) var sharing = false {
        didSet { changed() }
    }
    /// Why notifications cannot come, in words (nil when they can).
    @Published private(set) var problem: String?

    /// The accounts to tell, and the status log (set by the app model).
    var accounts: () -> [Account] = { [] }
    var note: (String) -> Void = { _ in }
    private var deviceToken: String?

    var wanted: Bool { presence || sharing }

    // MARK: When to act

    /// At launch: when on, the device token may have changed.
    func appStarted() {
        if wanted { Task { await enable() } }
    }

    /// After signing in to another account.
    func accountsChanged() {
        if wanted { Task { await syncAll() } }
    }

    /// After signing out (the server removed this iPhone, and with it its
    /// notifications there): a gateway no remaining account uses forgets
    /// this iPhone, and with no accounts left the notification key goes too.
    func signedOut(_ account: Account, remaining: [Account]) {
        let defaults = AccountStore.settings
        let server = Self.base(account.serverUrl)
        let remainingServers = Set(remaining.map { Self.base($0.serverUrl) })
        guard let gateway = defaults.string(forKey: "pushGateway " + server) else {
            if remaining.isEmpty { Self.deleteKey() }
            return
        }
        if !remainingServers.contains(server) {
            defaults.removeObject(forKey: "pushGateway " + server)
        }
        let stillUsed = remainingServers.contains { defaults.string(forKey: "pushGateway " + $0) == gateway }
        if !stillUsed, let stored = defaults.string(forKey: "pushHandle " + gateway) {
            defaults.removeObject(forKey: "pushHandle " + gateway)
            let parts = stored.split(separator: " ", maxSplits: 1).map(String.init)
            if parts.count == 2 {
                let handle = parts[1]
                Task {
                    _ = try? await call("POST", "\(Self.base(gateway))/push/v1/unregister",
                                        bearer: nil, body: ["handle": handle])
                }
            }
        }
        if remaining.isEmpty { Self.deleteKey() }
    }

    private static func base(_ url: String) -> String {
        var s = url
        while s.hasSuffix("/") { s.removeLast() }
        return s
    }

    private func changed() {
        objectWillChange.send()
        Task {
            if wanted { await enable() } else { await syncAll() }
        }
    }

    private func enable() async {
        let center = UNUserNotificationCenter.current()
        let granted = (try? await center.requestAuthorization(options: [.alert, .sound])) ?? false
        guard granted else {
            let text = "Notifications are turned off for AudioNet in the iPhone's Settings (Notifications, AudioNet)."
            problem = text
            note(text)
            return
        }
        problem = nil
        // The token arrives in gotToken, which tells the servers.
        UIApplication.shared.registerForRemoteNotifications()
        if deviceToken != nil { await syncAll() }
    }

    func gotToken(_ data: Data) {
        deviceToken = data.map { String(format: "%02x", $0) }.joined()
        Task { await syncAll() }
    }

    func tokenFailed(_ error: Error) {
        note("Notifications could not be set up: \(error.localizedDescription)")
    }

    // MARK: Telling the servers

    private func syncAll() async {
        for account in accounts() {
            do {
                try await sync(account)
            } catch {
                note("Notifications for \(account.accountName) could not be set: \(error.localizedDescription)")
            }
        }
    }

    private func sync(_ a: Account) async throws {
        var server = a.serverUrl
        while server.hasSuffix("/") { server.removeLast() }
        guard wanted else {
            _ = try await call("DELETE", "\(server)/api/v1/push/pusher", bearer: a.token, body: nil)
            return
        }
        guard let token = deviceToken else { return }
        let info = try await call("GET", "\(server)/api/v1/info", bearer: nil, body: nil)
        guard let gateway = info["push_gateway"] as? String, !gateway.isEmpty else {
            note("\(a.accountName): this server does not send notifications.")
            return
        }
        // Remembered so signing out can tell the gateway to forget us.
        AccountStore.settings.set(gateway, forKey: "pushGateway " + server)
        let handle = try await handle(gateway: gateway, deviceToken: token)
        _ = try await call("PUT", "\(server)/api/v1/push/pusher", bearer: a.token, body: [
            "gateway": gateway,
            "handle": handle,
            "key": Self.key().base64EncodedString(),
            "presence": presence,
            "sharing": sharing,
        ])
    }

    /// This iPhone's handle at a gateway, registered once per device token.
    private func handle(gateway: String, deviceToken: String) async throws -> String {
        let saved = "pushHandle " + gateway
        let defaults = AccountStore.settings
        if let stored = defaults.string(forKey: saved) {
            let parts = stored.split(separator: " ", maxSplits: 1).map(String.init)
            if parts.count == 2, parts[0] == deviceToken { return parts[1] }
        }
        var base = gateway
        while base.hasSuffix("/") { base.removeLast() }
        let answer = try await call("POST", "\(base)/push/v1/register", bearer: nil, body: [
            "apns_token": deviceToken,
            "sandbox": Self.sandbox,
        ])
        guard let handle = answer["handle"] as? String else {
            throw NotificationError(message: "the push gateway gave no handle")
        }
        defaults.set("\(deviceToken) \(handle)", forKey: saved)
        return handle
    }

    private func call(_ method: String, _ url: String, bearer: String?, body: [String: Any]?) async throws -> [String: Any] {
        guard let u = URL(string: url) else { throw NotificationError(message: "bad address \(url)") }
        var req = URLRequest(url: u)
        req.httpMethod = method
        if let bearer { req.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization") }
        if let body {
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
            req.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        let (data, response) = try await URLSession.shared.data(for: req)
        let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(status) else {
            let message = (json["error"] as? [String: Any])?["message"] as? String
            throw NotificationError(message: message ?? "the server answered \(status)")
        }
        return json
    }

    // MARK: This iPhone

    /// The key notifications are sealed with, made once and kept in the
    /// keychain group the notification extension shares.
    static func key() -> Data {
        let group = Bundle.main.object(forInfoDictionaryKey: "AudioNetKeychainGroup") as? String ?? ""
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "AudioNet notifications",
            kSecAttrAccount as String: keyName,
            kSecAttrAccessGroup as String: group,
        ]
        var found: CFTypeRef?
        var read = query
        read[kSecReturnData as String] = true
        if SecItemCopyMatching(read as CFDictionary, &found) == errSecSuccess,
           let key = found as? Data, key.count == 32 {
            return key
        }
        var bytes = [UInt8](repeating: 0, count: 32)
        _ = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        let key = Data(bytes)
        var add = query
        add[kSecValueData as String] = key
        // Readable while the iPhone is locked (after its first unlock), when
        // notifications arrive.
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemDelete(query as CFDictionary)
        SecItemAdd(add as CFDictionary, nil)
        return key
    }

    /// Removes the notification key (no accounts left).
    static func deleteKey() {
        let group = Bundle.main.object(forInfoDictionaryKey: "AudioNetKeychainGroup") as? String ?? ""
        SecItemDelete([
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "AudioNet notifications",
            kSecAttrAccount as String: keyName,
            kSecAttrAccessGroup as String: group,
        ] as CFDictionary)
    }

    /// Development builds (installed from a Mac) receive through Apple's
    /// sandbox; TestFlight and App Store builds do not.
    static var sandbox: Bool {
        guard let url = Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision"),
              let data = try? Data(contentsOf: url),
              let text = String(data: data, encoding: .isoLatin1)
        else { return false }
        return text.range(of: "<key>aps-environment</key>\\s*<string>development</string>",
                          options: .regularExpression) != nil
    }
}

struct NotificationError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

/// Receives the device token, and keeps notifications from appearing while
/// AudioNet is open.
final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        Task { @MainActor in Notifications.shared.gotToken(deviceToken) }
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        Task { @MainActor in Notifications.shared.tokenFailed(error) }
    }

    /// AudioNet is open: its own screen already says what changed.
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                            willPresent notification: UNNotification,
                                            withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([])
    }
}
