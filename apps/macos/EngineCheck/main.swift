// End-to-end check of the macOS app's engine (the same Rust core and Swift
// bindings the app uses), without the UI:
//   1. sign in as a test device and go online;
//   2. listen to SOURCE_DEVICE's sound whose name contains SOURCE_MATCH, on
//      this Mac's output whose name contains PLAY_ON; report diagnostics;
//   3. send this Mac's default input to SOURCE_DEVICE's output whose name
//      contains SEND_TO, for SEND_SECONDS.
// Settings come from a key=value file given as the first argument (deleted
// as soon as it is read: it holds the account password). Output goes to
// stdout and ~/Library/Logs/audionet-engine-check.log.
// Build: swiftc -I Generated -L Generated -laudionet_ffi <frameworks> Generated/audionet_ffi.swift EngineCheck/main.swift
import Foundation

let logURL = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Logs/audionet-engine-check.log")
FileManager.default.createFile(atPath: logURL.path, contents: nil)
let logHandle = try! FileHandle(forWritingTo: logURL)
/// Set once the settings are read; never written anywhere.
nonisolated(unsafe) var secret = ""
func say(_ s: String) {
    let line = secret.isEmpty ? s : s.replacingOccurrences(of: secret, with: "(password hidden)")
    print(line)
    logHandle.write((line + "\n").data(using: .utf8)!)
}

guard CommandLine.arguments.count > 1,
      let text = try? String(contentsOfFile: CommandLine.arguments[1], encoding: .utf8) else {
    say("FAIL: no settings file"); exit(2)
}
try? FileManager.default.removeItem(atPath: CommandLine.arguments[1])
var cfg: [String: String] = [:]
// Any line ending (Swift treats "\r\n" as a single character).
for raw in text.components(separatedBy: .newlines) {
    let line = raw.trimmingCharacters(in: .whitespacesAndNewlines)
    if let i = line.firstIndex(of: "=") { cfg[String(line[..<i])] = String(line[line.index(after: i)...]) }
}
secret = cfg["PASSWORD"] ?? ""
func setting(_ k: String) -> String { cfg[k] ?? "" }

final class Collector: EventListener, @unchecked Sendable {
    let lock = NSLock()
    var events: [Event] = []
    func onEvent(event: Event) {
        lock.lock(); events.append(event); lock.unlock()
        switch event {
        case .status(let t): say("  status: \(t)")
        case .diagnostics(_, let t): say("  diagnostics: \(t)")
        case .stream(_, let s, let d): say("  stream \(s): \(d)")
        case .streamEnded(_, let r): say("  stream ended: \(r)")
        case .serverProblem(let m): say("  server: \(m)")
        case .stopped(let e): say("  stopped \(e ?? "")")
        default: break
        }
    }
    func wait(_ what: String, seconds: Double, _ match: (Event) -> Bool) -> Event? {
        let end = Date().addingTimeInterval(seconds)
        while Date() < end {
            lock.lock(); let found = events.first(where: match); lock.unlock()
            if let found { return found }
            Thread.sleep(forTimeInterval: 0.1)
        }
        say("FAIL: timed out waiting for \(what)")
        return nil
    }
    func clear() { lock.lock(); events.removeAll(); lock.unlock() }
}

var ok = true
func check(_ name: String, _ pass: Bool, _ detail: String = "") {
    say("\(name): \(pass ? "PASS" : "FAIL")\(pass || detail.isEmpty ? "" : "  (\(detail))")")
    ok = ok && pass
}

say("engine \(engineVersion())")
let account: Account
do {
    account = try signIn(serverUrl: setting("URL"), username: setting("USER"), password: setting("PASSWORD"),
                         deviceName: setting("DEVICE_NAME"))
    check("signs in with the password", true)
} catch {
    check("signs in with the password", false, "\(error)"); exit(1)
}
let events = Collector()
let client = Client(account: account, listener: events)
client.start()
check("goes online", events.wait("connected", seconds: 30) { if case .connected = $0 { return true }; return false } != nil)

// Find the source device (online, with its sounds listed).
var source: Device?
_ = events.wait("the source device", seconds: 30) { e in
    let list: [Device]
    switch e {
    case .devices(let d): list = d
    case .deviceChanged(let d): list = [d]
    default: return false
    }
    source = list.first { $0.name == setting("SOURCE_DEVICE") && $0.online && !$0.sources.isEmpty }
    return source != nil
}
guard let src = source else { check("sees the source device", false); exit(1) }
check("sees the source device", true)
let local = try! localAudio()

// Listen.
let sound = src.sources.first { $0.name.contains(setting("SOURCE_MATCH")) }
let playOn = local.outputs.first { $0.name.contains(setting("PLAY_ON")) }

// REPEAT=N with REPEAT_OUTPUTS="name|name": listen N times on each output
// and time the startup steps (output opened, connected), then stop.
if let n = Int(setting("REPEAT")), n > 0, let sound {
    for name in setting("REPEAT_OUTPUTS").split(separator: "|").map(String.init) {
        guard let out = local.outputs.first(where: { $0.name.contains(name) }) else {
            say("REPEAT \(name): no such output"); continue
        }
        for i in 1...n {
            events.clear()
            let t0 = Date()
            let id = try! client.listen(nodeId: src.nodeId, sourceId: sound.id, outputId: out.id)
            let ready = events.wait("output opened", seconds: 30) {
                if case .stream(let s, _, let d) = $0 { return s == id && d.hasPrefix("Ready to play") }; return false
            }
            let t1 = Date()
            let active = ready == nil ? nil : events.wait("connected", seconds: 30) {
                if case .stream(let s, .active, _) = $0 { return s == id }; return false
            }
            let t2 = Date()
            let ms = { (a: Date, b: Date) in Int(b.timeIntervalSince(a) * 1000) }
            say("REPEAT \(out.name) #\(i): output opened \(ready == nil ? "TIMEOUT" : "\(ms(t0, t1)) ms"), "
                + "connected \(active == nil ? "no" : "\(ms(t0, t2)) ms")")
            client.stopStream(sessionId: id)
            _ = events.wait("ended", seconds: 15) { if case .streamEnded(let s, _) = $0 { return s == id }; return false }
        }
    }
    client.stop()
    say("DONE")
    exit(0)
}
if let sound, let playOn {
    events.clear()
    let id = try! client.listen(nodeId: src.nodeId, sourceId: sound.id, outputId: playOn.id)
    check("listen connects", events.wait("listen connected", seconds: 30) {
        if case .stream(let s, .active, _) = $0 { return s == id }; return false } != nil)
    Thread.sleep(forTimeInterval: 7)
    events.lock.lock()
    let diag = events.events.compactMap { e -> String? in if case .diagnostics(let s, let t) = e, s == id { return t }; return nil }.last ?? ""
    events.lock.unlock()
    say("  last diagnostics: \(diag)")
    let packets = Int(diag.split(separator: " ").dropFirst().first ?? "0") ?? 0
    check("audio arrives and plays without underruns", packets > 200 && diag.contains("underruns 0"), diag)
    client.stopStream(sessionId: id)
    check("listen stops", events.wait("listen ended", seconds: 15) {
        if case .streamEnded(let s, _) = $0 { return s == id }; return false } != nil)
} else {
    check("finds the sound and output", false, "sound \(sound?.name ?? "none"), output \(playOn?.name ?? "none")")
}

// Send. SEND_FROM picks a source by name: an input, or "Sound playing on
// <output>" for system audio (a closed MacBook's microphone is disconnected
// in hardware and records silence); otherwise the default input. With
// SEND_FROM set, the source sound keeps playing on PLAY_ON meanwhile, so
// sending what PLAY_ON plays (or a loopback device's input) closes the loop
// back to the source device.
let sendFrom = setting("SEND_FROM")
let mic = sendFrom.isEmpty
    ? (local.sources.first { $0.isInput && $0.isDefault } ?? local.sources.first { $0.isInput })
    : (local.sources.first { $0.name == sendFrom } ?? local.sources.first { $0.name.contains(sendFrom) })
var feed: String?
if !sendFrom.isEmpty, let sound, let playOn {
    feed = try! client.listen(nodeId: src.nodeId, sourceId: sound.id, outputId: playOn.id)
    _ = events.wait("feed connected", seconds: 30) { if case .stream(let s, .active, _) = $0 { return s == feed }; return false }
}
let to = src.outputs.first { $0.name.contains(setting("SEND_TO")) }
if let mic, let to {
    events.clear()
    say("  sending \(mic.name) to \(to.name)")
    let id = try! client.send(nodeId: src.nodeId, sourceId: mic.id, outputId: to.id)
    say("SEND_STARTED \(Date().timeIntervalSince1970)")
    check("send connects", events.wait("send connected", seconds: 30) {
        if case .stream(let s, .active, _) = $0 { return s == id }; return false } != nil)
    say("SEND_CONNECTED \(Date().timeIntervalSince1970)")
    Thread.sleep(forTimeInterval: Double(setting("SEND_SECONDS")) ?? 12)
    events.lock.lock()
    let warned = events.events.contains { if case .stream(let s, _, let d) = $0 { return s == id && d.hasPrefix("Warning:") }; return false }
    events.lock.unlock()
    check("the microphone is not silent", !warned)
    client.stopStream(sessionId: id)
    say("SEND_STOPPED \(Date().timeIntervalSince1970)")
    if let feed { client.stopStream(sessionId: feed) }
} else {
    check("finds the microphone and the output", false, "mic \(mic?.name ?? "none"), output \(to?.name ?? "none")")
}
client.stop()
say(ok ? "RESULT: PASS" : "RESULT: FAIL")
say("DONE")
