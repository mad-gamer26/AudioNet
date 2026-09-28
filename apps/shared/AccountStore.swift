import Foundation
import Security

/// Where this device's AudioNet memberships are kept: for each account it
/// is signed in to, the server, device id, device token and names, in
/// `~/Library/Application Support/AudioNet/accounts.json`, readable and
/// writable only by this user (0600, in a 0700 folder), the way the Windows
/// app keeps its settings file. Account passwords are never stored. Each
/// device token is revocable: removing the device on the web page ends it.
///
/// An app can be signed in to several accounts at once; it is then a
/// separate device in each (one device token per account).
///
/// Not the keychain: every AudioNet build is ad-hoc signed, so macOS saw
/// each update as a different app and asked for the login password before
/// it could read the token (an unanswered prompt even made an update roll
/// back). The user chose a user-only file instead.
///
/// Tests launch the app with `-AudioNetProfile NAME`, which keeps their
/// accounts in a separate file and preferences store, so they never touch
/// the accounts of someone using AudioNet on this device.
enum AccountStore {
    private static var profile: String? {
        UserDefaults.standard.string(forKey: "AudioNetProfile").flatMap { $0.isEmpty ? nil : $0 }
    }
    private static var defaults: UserDefaults { settings }

    /// Running under a test profile (UI tests).
    static var isTestProfile: Bool { profile != nil }

    /// The app's preferences: the test profile's own store in tests.
    static var settings: UserDefaults {
        profile.flatMap { UserDefaults(suiteName: "org.audionet.AudioNet.test.\($0)") } ?? .standard
    }

    private struct Stored: Codable {
        var server: String
        var nodeId: String
        var token: String
        var deviceName: String
        var username: String

        init(_ a: Account) {
            server = a.serverUrl
            nodeId = a.nodeId
            token = a.token
            deviceName = a.deviceName
            username = a.username
        }

        var account: Account {
            Account(serverUrl: server, nodeId: nodeId, token: token, deviceName: deviceName, username: username)
        }
    }

    private static var folder: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("AudioNet", isDirectory: true)
    }

    static var fileURL: URL {
        folder.appendingPathComponent(profile.map { "accounts-test-\($0).json" } ?? "accounts.json")
    }

    /// Versions up to 0.5.8 kept one account in this file.
    private static var singleFileURL: URL {
        folder.appendingPathComponent(profile.map { "account-test-\($0).json" } ?? "account.json")
    }

    /// Test profiles only (never the real accounts): `-AudioNetResetProfile
    /// YES` starts signed out, and `-AudioNetTestAccountFile PATH` signs in
    /// with devices a test created, from a key=value file (server, node_id,
    /// token, device_name, username; several accounts separated by a line
    /// "---") that is deleted as soon as it is read.
    static func prepareTestProfile() {
        guard profile != nil else { return }
        let args = UserDefaults.standard
        if args.bool(forKey: "AudioNetResetProfile") { clear() }
        // From a file (deleted once read), or, where a test cannot place
        // files (the iOS simulator), the AUDIONET_TEST_ACCOUNT environment
        // variable. Test profiles only; the devices are temporary ones.
        let text: String
        if let path = args.string(forKey: "AudioNetTestAccountFile"), !path.isEmpty,
           let fileText = try? String(contentsOfFile: path, encoding: .utf8) {
            try? FileManager.default.removeItem(atPath: path)
            text = fileText
        } else if let env = ProcessInfo.processInfo.environment["AUDIONET_TEST_ACCOUNT"], !env.isEmpty {
            text = env
        } else {
            return
        }
        var accounts: [Account] = []
        var v: [String: String] = [:]
        func take() {
            if let s = v["server"], let n = v["node_id"], let t = v["token"], let d = v["device_name"], let u = v["username"] {
                accounts.append(Account(serverUrl: s, nodeId: n, token: t, deviceName: d, username: u))
            }
            v = [:]
        }
        for raw in text.components(separatedBy: .newlines) {
            let line = raw.trimmingCharacters(in: .whitespaces)
            if line == "---" { take(); continue }
            if let i = line.firstIndex(of: "=") { v[String(line[..<i])] = String(line[line.index(after: i)...]) }
        }
        take()
        if !accounts.isEmpty { _ = saveAll(accounts) }
    }

    /// Writes the accounts file, only this user may read it. Returns why it
    /// failed, in words.
    @discardableResult
    static func saveAll(_ accounts: [Account]) -> String? {
        let fm = FileManager.default
        do {
            try fm.createDirectory(at: folder, withIntermediateDirectories: true,
                                   attributes: [.posixPermissions: 0o700])
            try fm.setAttributes([.posixPermissions: 0o700], ofItemAtPath: folder.path)
            let data = try JSONEncoder().encode(accounts.map(Stored.init))
            // Created 0600 before anything is written, then moved into place.
            let temp = folder.appendingPathComponent(".accounts-\(UUID().uuidString).tmp")
            guard fm.createFile(atPath: temp.path, contents: nil, attributes: [.posixPermissions: 0o600]) else {
                return "could not create \(temp.path)"
            }
            let handle = try FileHandle(forWritingTo: temp)
            try handle.write(contentsOf: data)
            try handle.close()
            _ = try fm.replaceItemAt(fileURL, withItemAt: temp)
            try fm.setAttributes([.posixPermissions: 0o600], ofItemAtPath: fileURL.path)
            return nil
        } catch {
            return "AudioNet could not save this device's sign-ins in \(fileURL.path): \(error.localizedDescription)"
        }
    }

    /// Whether this device is signed in to any account (a file, or an older
    /// version's sign-in still to be moved).
    static var hasStoredAccount: Bool {
        let fm = FileManager.default
        return fm.fileExists(atPath: fileURL.path) || fm.fileExists(atPath: singleFileURL.path)
            || defaults.string(forKey: "nodeId") != nil
    }

    /// The stored accounts, and why some could not be read (in words). Safe
    /// on any thread (a one-time move from the keychain can wait for a
    /// macOS prompt, so the apps call it off the main thread).
    static func loadAll() -> (accounts: [Account], problem: String?) {
        let fm = FileManager.default
        if let data = fm.contents(atPath: fileURL.path) {
            // Keep it private even if something loosened the permissions.
            if let mode = (try? fm.attributesOfItem(atPath: fileURL.path))?[.posixPermissions] as? Int, mode & 0o077 != 0 {
                try? fm.setAttributes([.posixPermissions: 0o600], ofItemAtPath: fileURL.path)
            }
            guard let s = try? JSONDecoder().decode([Stored].self, from: data) else {
                return ([], "AudioNet's sign-in file (\(fileURL.path)) is damaged. Sign in again.")
            }
            return (s.map(\.account), nil)
        }
        // An older version's single account: moved into the list once.
        if let data = fm.contents(atPath: singleFileURL.path) {
            guard let s = try? JSONDecoder().decode(Stored.self, from: data) else {
                return ([], "AudioNet's sign-in file (\(singleFileURL.path)) is damaged. Sign in again.")
            }
            if let problem = saveAll([s.account]) { return ([s.account], problem) }
            try? fm.removeItem(at: singleFileURL)
            return ([s.account], nil)
        }
        let (account, problem) = moveFromKeychain()
        return (account.map { [$0] } ?? [], problem)
    }

    // MARK: Versions up to 0.5.1 kept the token in the keychain

    private static var legacyService: String {
        profile.map { "org.audionet.AudioNet.device.test.\($0)" } ?? "org.audionet.AudioNet.device"
    }

    private static let legacyKeys = ["server", "nodeId", "deviceName", "username"]

    /// Moves an older version's sign-in (preferences + keychain token) into
    /// the file, once. macOS may ask one last time for the keychain.
    private static func moveFromKeychain() -> (account: Account?, problem: String?) {
        guard let server = defaults.string(forKey: "server"),
              let nodeId = defaults.string(forKey: "nodeId"),
              let deviceName = defaults.string(forKey: "deviceName"),
              let username = defaults.string(forKey: "username") else { return (nil, nil) }
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: legacyService,
            kSecAttrAccount as String: nodeId,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard status == errSecSuccess, let data = item as? Data, let token = String(data: data, encoding: .utf8) else {
            let why = (SecCopyErrorMessageString(status, nil) as String?) ?? "error \(status)"
            return (nil, "AudioNet could not move this device's sign-in out of the keychain (\(why)). "
                + "Open AudioNet again and allow keychain access once, or sign in again.")
        }
        let account = Account(serverUrl: server, nodeId: nodeId, token: token, deviceName: deviceName, username: username)
        if let problem = saveAll([account]) { return (account, problem) }
        removeLegacy(nodeId)
        return (account, nil)
    }

    private static func removeLegacy(_ nodeId: String) {
        let q: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: legacyService,
            kSecAttrAccount as String: nodeId,
        ]
        SecItemDelete(q as CFDictionary)
        for key in legacyKeys { defaults.removeObject(forKey: key) }
    }

    /// Forgets every account on this device.
    static func clear() {
        try? FileManager.default.removeItem(at: fileURL)
        try? FileManager.default.removeItem(at: singleFileURL)
        // Test profiles may still hold an older version's entries.
        if let nodeId = defaults.string(forKey: "nodeId") { removeLegacy(nodeId) }
    }
}

extension Account {
    /// The account in words: "mad-gamer26 on audionet.example.com".
    var accountName: String {
        let host = URL(string: serverUrl)?.host ?? serverUrl
        return "\(username) on \(host)"
    }

    /// Whether `other` is the same account (same server and name).
    func sameAccount(as other: Account) -> Bool {
        username.lowercased() == other.username.lowercased()
            && serverUrl.trimmingCharacters(in: CharacterSet(charactersIn: "/")).lowercased()
            == other.serverUrl.trimmingCharacters(in: CharacterSet(charactersIn: "/")).lowercased()
    }
}
