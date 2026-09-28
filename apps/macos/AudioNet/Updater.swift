import AppKit
import CryptoKit
import Security
import Foundation
import SwiftUI

/// Signed automatic updates, the same scheme as the Windows app (see
/// docs/releasing.md):
///
/// 1. `latest-macos.json` next to the configured manifest URL names the
///    version, the zip, its SHA-256 and size, and must carry a valid Ed25519
///    signature (`latest-macos.json.sig`) from the release key built into
///    the app. Anything unsigned, altered, not newer, for another product,
///    or not over HTTPS is ignored.
/// 2. The zip is downloaded, checked against the signed size and SHA-256,
///    unpacked next to the app, and deleted. The unpacked app must have this
///    app's bundle identifier, the signed version and a valid code signature.
/// 3. When no stream is running, a small helper swaps the app bundles after
///    AudioNet quits and starts the new copy, online again if it was. If the
///    new copy does not report within 20 seconds, the helper puts the old one
///    back and starts it, and that version is not offered again.
///
/// Builds without an update URL and key (plain source builds) never update.
@MainActor
final class Updater: ObservableObject {
    static let product = "audionet-macos-universal"
    static let maxPackageBytes = 300 * 1024 * 1024

    @Published private(set) var status = ""
    @AppStorage("autoUpdate", store: AccountStore.settings) var automatic = true
    /// A version that was installed but did not start; never offered again.
    @AppStorage("skipUpdateVersion", store: AccountStore.settings) private var skipVersion = ""

    let manifestURL: URL?
    private let publicKey: Curve25519.Signing.PublicKey?
    private var checking = false
    private var timer: Timer?
    weak var model: AppModel?

    struct Manifest: Decodable {
        let schema: Int
        let product: String
        let version: String
        let file: String
        let sha256: String
        let size: Int
    }

    struct Failure: Error { let message: String }

    init() {
        let info = Bundle.main.infoDictionary ?? [:]
        let base = (info["AudioNetUpdateURL"] as? String).flatMap { $0.isEmpty ? nil : URL(string: $0) }
        // The Mac manifest sits next to the configured (Windows) one.
        manifestURL = base.map { $0.deletingLastPathComponent().appendingPathComponent("latest-macos.json") }
        publicKey = (info["AudioNetUpdatePublicKey"] as? String)
            .flatMap(Data.init(hex:))
            .flatMap { try? Curve25519.Signing.PublicKey(rawRepresentation: $0) }
    }

    var configured: Bool { manifestURL != nil && publicKey != nil }

    static var currentVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.0.0"
    }

    /// Checks 30 seconds after start, then every 6 hours (hourly after a
    /// failure), while automatic updates are on.
    func start() {
        guard configured else {
            status = "This build does not update itself."
            return
        }
        schedule(after: 30)
    }

    private func schedule(after seconds: TimeInterval) {
        timer?.invalidate()
        timer = Timer.scheduledTimer(withTimeInterval: seconds, repeats: false) { _ in
            Task { @MainActor in
                if self.automatic {
                    await self.check(userAsked: false)
                } else {
                    self.schedule(after: 6 * 3600)
                }
            }
        }
    }

    // MARK: Checking

    func check(userAsked: Bool) async {
        guard configured, let manifestURL, let publicKey, !checking else { return }
        checking = true
        defer { checking = false }
        say("Checking for updates.", announce: userAsked)
        do {
            let m = try await fetchManifest(manifestURL, publicKey)
            guard Self.isNewer(m.version, than: Self.currentVersion), m.version != skipVersion else {
                say("AudioNet is up to date (version \(Self.currentVersion)).", announce: userAsked)
                schedule(after: 6 * 3600)
                return
            }
            say("Downloading AudioNet \(m.version).", announce: userAsked)
            let staged = try await download(m, from: manifestURL)
            say("AudioNet \(m.version) is ready. It will be installed when no stream is running.", announce: true)
            await installWhenIdle(staged, version: m.version)
        } catch let f as Failure {
            say("Updating failed: \(f.message)", announce: userAsked)
            schedule(after: 3600)
        } catch {
            say("Updating failed: \(error.localizedDescription)", announce: userAsked)
            schedule(after: 3600)
        }
    }

    private func say(_ text: String, announce: Bool) {
        status = text
        // Kept for diagnostics (and the update test): the latest outcome.
        AccountStore.settings.set(text, forKey: "updateStatus")
        if announce { model?.announce(text) } else { model?.note(text) }
    }

    static func isNewer(_ candidate: String, than current: String) -> Bool {
        func parts(_ s: String) -> [Int]? {
            let p = s.split(separator: ".").map { Int($0) }
            return p.count == 3 && !p.contains(nil) ? p.compactMap { $0 } : nil
        }
        guard let a = parts(candidate), let b = parts(current) else { return false }
        return a.lexicographicallyPrecedes(b) == false && a != b
    }

    private static func checkURL(_ url: URL) throws {
        let s = url.absoluteString
        guard s.hasPrefix("https://") || s.hasPrefix("http://127.0.0.1:") || s.hasPrefix("http://localhost:") else {
            throw Failure(message: "updates must come over HTTPS, not \(s)")
        }
    }

    private static func get(_ url: URL, limit: Int) async throws -> Data {
        try checkURL(url)
        var request = URLRequest(url: url)
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.timeoutInterval = 30
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            throw Failure(message: "the update server answered \((response as? HTTPURLResponse)?.statusCode ?? 0) for \(url.lastPathComponent)")
        }
        guard data.count <= limit else { throw Failure(message: "\(url.lastPathComponent) is too large") }
        return data
    }

    /// Verifies the signature over the exact manifest bytes, then its fields.
    private func fetchManifest(_ url: URL, _ key: Curve25519.Signing.PublicKey) async throws -> Manifest {
        let body = try await Self.get(url, limit: 64 * 1024)
        let sigText = try await Self.get(url.appendingPathExtension("sig"), limit: 1024)
        guard let sig = String(data: sigText, encoding: .utf8).flatMap(Data.init(hex:)), sig.count == 64 else {
            throw Failure(message: "the update signature is malformed")
        }
        guard key.isValidSignature(sig, for: body) else {
            throw Failure(message: "the update is not signed by the AudioNet release key; it was ignored")
        }
        let m: Manifest
        do { m = try JSONDecoder().decode(Manifest.self, from: body) } catch {
            throw Failure(message: "the update description is invalid")
        }
        guard m.schema == 1 else { throw Failure(message: "the update description uses an unknown format (\(m.schema))") }
        guard m.product == Self.product else { throw Failure(message: "the update is for \(m.product), not \(Self.product)") }
        guard Self.isNewer(m.version, than: "0.0.0") else { throw Failure(message: "the update has an invalid version") }
        guard m.file.hasSuffix(".zip"), !m.file.hasPrefix("."), !m.file.contains("/"), !m.file.contains("\\") else {
            throw Failure(message: "the update names an invalid package file")
        }
        guard Data(hex: m.sha256)?.count == 32 else { throw Failure(message: "the update has an invalid checksum") }
        guard m.size > 0, m.size <= Self.maxPackageBytes else { throw Failure(message: "the update package size is out of range") }
        return m
    }

    // MARK: Downloading and unpacking

    private var appURL: URL { Bundle.main.bundleURL }

    /// Downloads and checks the zip, unpacks it next to the app and returns
    /// the unpacked app.
    private func download(_ m: Manifest, from manifestURL: URL) async throws -> URL {
        let url = manifestURL.deletingLastPathComponent().appendingPathComponent(m.file)
        try Self.checkURL(url)
        let parent = appURL.deletingLastPathComponent()
        guard FileManager.default.isWritableFile(atPath: parent.path),
              FileManager.default.isWritableFile(atPath: appURL.path) else {
            throw Failure(message: "AudioNet cannot replace itself in \(parent.path) (no permission). Move AudioNet to Applications or your own folder.")
        }
        let (tmp, response) = try await URLSession.shared.download(from: url)
        defer { try? FileManager.default.removeItem(at: tmp) }
        guard (response as? HTTPURLResponse)?.statusCode == 200 else {
            throw Failure(message: "the update package could not be downloaded")
        }
        let data = try Data(contentsOf: tmp, options: .mappedIfSafe)
        guard data.count == m.size else { throw Failure(message: "the downloaded update has the wrong size") }
        let digest = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
        guard digest == m.sha256.lowercased() else { throw Failure(message: "the downloaded update is damaged (checksum)") }

        let staging = parent.appendingPathComponent(".AudioNet-update-\(m.version)")
        try? FileManager.default.removeItem(at: staging)
        try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: false)
        do {
            try run("/usr/bin/ditto", ["-x", "-k", tmp.path, staging.path])
            let contents = try FileManager.default.contentsOfDirectory(atPath: staging.path)
            guard contents == ["AudioNet.app"] else { throw Failure(message: "the update package does not hold AudioNet.app alone") }
            let app = staging.appendingPathComponent("AudioNet.app")
            guard let bundle = Bundle(url: app),
                  bundle.bundleIdentifier == Bundle.main.bundleIdentifier,
                  bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String == m.version else {
                throw Failure(message: "the update package holds a different app or version")
            }
            try run("/usr/bin/codesign", ["--verify", "--deep", "--strict", app.path])
            // A Developer ID signed AudioNet accepts only updates signed by
            // the same Apple team (on top of the release key's signature).
            if let team = Self.teamID {
                try run("/usr/bin/codesign", ["--verify", "--deep", "--strict",
                                              "-R=anchor apple generic and certificate leaf[subject.OU] = \"\(team)\"",
                                              app.path])
            }
            return app
        } catch {
            try? FileManager.default.removeItem(at: staging)
            throw error
        }
    }

    private func run(_ tool: String, _ args: [String]) throws {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: tool)
        p.arguments = args
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try p.run()
        p.waitUntilExit()
        guard p.terminationStatus == 0 else {
            throw Failure(message: "\(URL(fileURLWithPath: tool).lastPathComponent) failed on the update package")
        }
    }

    /// The Apple team that signed this copy (nil for ad-hoc builds).
    static let teamID: String? = {
        var code: SecCode?
        guard SecCodeCopySelf([], &code) == errSecSuccess, let code else { return nil }
        var staticCode: SecStaticCode?
        guard SecCodeCopyStaticCode(code, [], &staticCode) == errSecSuccess, let staticCode else { return nil }
        var info: CFDictionary?
        guard SecCodeCopySigningInformation(staticCode, SecCSFlags(rawValue: kSecCSSigningInformation), &info) == errSecSuccess,
              let dict = info as? [String: Any] else { return nil }
        return dict[kSecCodeInfoTeamIdentifier as String] as? String
    }()

    // MARK: Installing

    private func installWhenIdle(_ staged: URL, version: String) async {
        while model?.busyStreaming == true {
            try? await Task.sleep(for: .seconds(60))
        }
        do {
            try install(staged, version: version)
        } catch {
            try? FileManager.default.removeItem(at: staged.deletingLastPathComponent())
            say("Updating failed: \(describe(error))", announce: true)
            schedule(after: 3600)
        }
    }

    /// Starts the helper that swaps the bundles once AudioNet has quit, then
    /// quits.
    private func install(_ staged: URL, version: String) throws {
        let fm = FileManager.default
        let work = fm.temporaryDirectory.appendingPathComponent("audionet-update-\(UUID().uuidString)")
        try fm.createDirectory(at: work, withIntermediateDirectories: true)
        let script = work.appendingPathComponent("install.sh")
        try Self.helper.write(to: script, atomically: true, encoding: .utf8)
        let backup = appURL.deletingLastPathComponent().appendingPathComponent(".AudioNet-previous.app")
        let marker = work.appendingPathComponent("started")
        var relaunch = ["-AudioNetUpdatedFrom", Self.currentVersion, "-AudioNetUpdateExpected", version,
                        "-AudioNetUpdateMarker", marker.path]
        // Each account's sharing is saved; this matters only to a copy from
        // before that (which then shares everywhere).
        if model?.sharingAccounts.isEmpty == false { relaunch += ["-AudioNetGoOnline", "YES"] }
        if !(NSApp.windows.contains { $0.isVisible && $0.identifier?.rawValue.hasPrefix("main") == true }) {
            relaunch += ["-AudioNetStartHidden", "YES"]
        }
        // A test profile stays in use across the restart (and a rollback).
        let profile = UserDefaults.standard.string(forKey: "AudioNetProfile") ?? ""
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        p.arguments = [script.path, String(ProcessInfo.processInfo.processIdentifier), appURL.path,
                       staged.path, backup.path, marker.path, version, work.path, profile] + relaunch
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try p.run()
        say("Installing AudioNet \(version). AudioNet restarts by itself.", announce: true)
        model?.disconnectAll()
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { NSApp.terminate(nil) }
    }

    /// $1 pid of the running copy, $2 app path, $3 unpacked new app,
    /// $4 backup path, $5 marker the new copy writes, $6 new version,
    /// $7 work folder, $8 test profile or ""; the rest are arguments for
    /// the new copy.
    static let helper = #"""
    #!/bin/sh
    pid=$1 app=$2 new=$3 backup=$4 marker=$5 version=$6 work=$7 profile=$8
    shift 8
    if [ -n "$profile" ]; then set -- "$@" -AudioNetProfile "$profile"; fi
    start_old() {
        if [ -n "$profile" ]; then open "$app" --args -AudioNetProfile "$profile" "$@"; else open "$app" --args "$@"; fi
    }
    i=0
    while kill -0 "$pid" 2>/dev/null; do
        i=$((i + 1)); [ $i -gt 300 ] && exit 1
        sleep 0.1
    done
    staging=$(dirname "$new")
    rm -rf "$backup"
    if ! mv "$app" "$backup"; then rm -rf "$staging" "$work"; start_old; exit 1; fi
    if ! mv "$new" "$app"; then mv "$backup" "$app"; rm -rf "$staging" "$work"; start_old; exit 1; fi
    rm -rf "$staging"
    open -n "$app" --args "$@"
    i=0
    while [ ! -f "$marker" ] && [ $i -lt 200 ]; do i=$((i + 1)); sleep 0.1; done
    if [ -f "$marker" ]; then
        rm -rf "$backup" "$work"
        exit 0
    fi
    # The new copy did not start: put the old one back.
    pkill -f "$app/Contents/MacOS/" 2>/dev/null
    sleep 1
    rm -rf "$app"
    mv "$backup" "$app"
    rm -rf "$work"
    start_old -AudioNetUpdateFailed "$version"
    """#

    /// Called at launch: reports an update that just happened or failed.
    /// Called first thing at launch, before anything that can wait for the
    /// person (a keychain or permission dialog): tells the update helper
    /// that this new copy started, so it is not rolled back while a dialog
    /// is open.
    static func reportStarted() {
        if let marker = UserDefaults.standard.string(forKey: "AudioNetUpdateMarker"), !marker.isEmpty {
            FileManager.default.createFile(atPath: marker, contents: Data("ok".utf8))
        }
    }

    func afterLaunch() {
        let args = UserDefaults.standard
        if let from = args.string(forKey: "AudioNetUpdatedFrom"), !from.isEmpty {
            let expected = args.string(forKey: "AudioNetUpdateExpected") ?? ""
            if !expected.isEmpty && expected != Self.currentVersion {
                // A mislabeled release: never offer it again.
                skipVersion = expected
            }
            say("AudioNet was updated from version \(from) to \(Self.currentVersion).", announce: true)
        }
        if let failed = args.string(forKey: "AudioNetUpdateFailed"), !failed.isEmpty {
            skipVersion = failed
            say("AudioNet \(failed) did not start, so version \(Self.currentVersion) was put back. That update will not be offered again.", announce: true)
        }
    }
}

extension Data {
    init?(hex: String) {
        let s = hex.trimmingCharacters(in: .whitespacesAndNewlines)
        guard s.count % 2 == 0 else { return nil }
        var bytes = [UInt8]()
        bytes.reserveCapacity(s.count / 2)
        var i = s.startIndex
        while i < s.endIndex {
            let j = s.index(i, offsetBy: 2)
            guard let b = UInt8(s[i..<j], radix: 16) else { return nil }
            bytes.append(b)
            i = j
        }
        self.init(bytes)
    }
}
