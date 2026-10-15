import AVFoundation
import os

/// One way to record: an input port (the built-in microphone, a wired
/// headset) and, where the port has several, one of its data sources (the
/// built-in microphone's bottom, front or back). Bluetooth microphones are
/// not offered: AudioNet never uses Bluetooth HFP.
struct Microphone: Identifiable, Hashable, Sendable {
    /// "portUID::dataSourceID" ("default" for a port without data sources).
    let id: String
    /// The data source ("Front"), or "Default".
    let name: String
    /// Port and source, for saying which microphone is chosen
    /// ("iPhone Microphone, Front").
    let fullName: String
    /// The source can record in stereo (a stereo polar pattern).
    let stereo: Bool
}

/// An input port and its microphones, as UniMic lists them.
struct MicrophoneGroup: Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let microphones: [Microphone]
}

/// The iPhone's audio session. AudioNet plays alongside other apps' audio;
/// it never stops or lowers a podcast or music.
///
/// Like UniMic, the session is active only while AudioNet has a stream:
/// the engine reports its streams (``AudioUse``) before one opens and after
/// one closes, and the session follows.
/// - Nothing open: inactive, the phone's audio untouched.
/// - Playing only: playback, which never involves the microphone.
/// - Recording: play and record with the chosen microphone as the preferred
///   input, in stereo where the microphone can (the data source's stereo
///   polar pattern and two input channels, as UniMic sets them), at 48 kHz
///   (asked for only when recording: a playback stream must not open while
///   the rate changes),
///   with no stereo orientation set (as UniMic: iOS's default). Never
///   Bluetooth HFP: Bluetooth headphones keep playing in full quality
///   (A2DP) and their microphones are not used.
/// - Changing the microphone while recording takes effect at once.
/// - The last microphone stream ends: the preferred input is released and
///   the session returns to playback (or becomes inactive), so the
///   microphone is let go at once.
///
/// A running stream survives these changes: iOS reroutes it, and the engine
/// reopens a stream only when the route's sample rate or channels changed.
enum AudioSession {
    private static var session: AVAudioSession { .sharedInstance() }

    private struct State: Sendable {
        var online = false
        /// The chosen microphone; nil: the one the iPhone uses now.
        var microphone: Microphone?
        var streams: UInt32 = 0
        var record = false
        var sessionActive = false
    }
    private static let state = OSAllocatedUnfairLock(initialState: State())

    /// The microphone to record from (nil: the one the iPhone uses now).
    /// While recording it takes over at once: iOS moves the running capture
    /// to it (the engine reopens the capture only if the format changed).
    /// May wait for the route: not on the main thread.
    static func setMicrophone(_ m: Microphone?) {
        state.withLock { s in
            s.microphone = m
            if s.record && s.sessionActive {
                configure(s)
                useMicrophone(s)
            }
        }
    }

    /// Going online: streams may start from now on.
    static func goOnline() {
        state.withLock { $0.online = true }
    }

    /// Going offline: hands the audio back.
    static func goOffline() {
        state.withLock { s in
            s.online = false
            s.streams = 0
            release(&s)
        }
    }

    /// The engine's streams changed: `streams` open (a new one already
    /// counted, not yet opened), `microphones` of them recording.
    static func streamsChanged(streams: UInt32, microphones: UInt32) {
        state.withLock { s in
            s.streams = streams
            guard s.online, streams > 0 else {
                release(&s)
                return
            }
            let record = microphones > 0
            guard record != s.record || !s.sessionActive else { return }
            if s.record && !record { try? session.setPreferredInput(nil) }
            s.record = record
            configure(s)
            if !s.sessionActive {
                do {
                    try session.setActive(true)
                    s.sessionActive = true
                } catch {
                    report("The iPhone's audio could not be started: \(error.localizedDescription)")
                }
            }
            if record { useMicrophone(s) }
        }
    }

    /// No streams: the microphone and the session are let go entirely.
    private static func release(_ s: inout State) {
        let wasActive = s.sessionActive
        s.record = false
        s.sessionActive = false
        try? session.setPreferredInput(nil)
        if wasActive {
            try? session.setActive(false, options: .notifyOthersOnDeactivation)
        }
        configure(s)
    }

    private static func configure(_ s: State) {
        var options: AVAudioSession.CategoryOptions = [.mixWithOthers]
        let category: AVAudioSession.Category = s.record ? .playAndRecord : .playback
        if s.record {
            options.formUnion([.defaultToSpeaker, .allowBluetoothA2DP])
        }
        guard session.category != category || session.categoryOptions != options else { return }
        do {
            try session.setCategory(category, mode: .default, options: options)
        } catch {
            report("The iPhone's audio could not be set up: \(error.localizedDescription)")
        }
    }

    /// Records from `m` (or the current input), in stereo where it can, as
    /// UniMic does: the preferred input, the data source with its stereo
    /// polar pattern, then two input channels. Waits (at most 1.5 seconds)
    /// until the route uses that input in that many channels, so the capture
    /// opens in the final format instead of being reopened.
    private static func useMicrophone(_ s: State) {
        let m = s.microphone
        let wanted = m.map { parse($0.id) }
        // The chosen input, if the iPhone has it now (never a Bluetooth one).
        let chosenPort = wanted.flatMap { w in
            (session.availableInputs ?? []).first { $0.uid == w.port && !isBluetooth($0) }
        }
        if chosenPort == nil {
            // The iPhone chooses: release an earlier choice and let the
            // route settle before reading it.
            if session.preferredInput != nil {
                try? session.setPreferredInput(nil)
                let end = Date().addingTimeInterval(0.5)
                while Date() < end, session.preferredInput != nil { Thread.sleep(forTimeInterval: 0.02) }
            }
            if m != nil { NSLog("AudioNet: the chosen microphone %@ is not available; the iPhone chooses", m?.fullName ?? "") }
        }
        // The input in use, as listed (with its data sources).
        let routed = session.currentRoute.inputs.first.flatMap { r in
            (session.availableInputs ?? []).first { $0.uid == r.uid } ?? r
        }
        guard let port = chosenPort ?? routed else { return }
        // Apple's stereo recipe (and UniMic's): the input and its data source
        // chosen explicitly, the stereo polar pattern set on the source
        // first; no input orientation is set (as UniMic). With "the iPhone
        // chooses", the current input and its current source.
        let chosenSource = wanted?.source.flatMap { id in port.dataSources?.first { $0.dataSourceID.description == id } }
        // Nothing chosen: the built-in microphone's default source (Bottom on
        // an iPhone 16 Pro) is mono only; record in stereo from a source
        // that can, the front one (facing the person) first.
        let stereoSources = (port.dataSources ?? []).filter { $0.supportedPolarPatterns?.contains(.stereo) == true }
        let stereoDefault = port.portType == .builtInMic
            ? stereoSources.first { $0.orientation == .front } ?? stereoSources.first
            : nil
        let source = chosenSource ?? stereoDefault ?? port.selectedDataSource ?? port.preferredDataSource
        // 48 kHz for recording (Opus's rate), as UniMic asks; only here, so
        // a playback stream never opens while the rate is changing. The
        // capture opens once the rate is in place (at most half a second).
        if session.sampleRate != 48_000, (try? session.setPreferredSampleRate(48_000)) != nil {
            let end = Date().addingTimeInterval(0.5)
            while Date() < end, session.sampleRate != 48_000 { Thread.sleep(forTimeInterval: 0.02) }
        }
        var stereo = false
        do {
            try session.setPreferredInput(port)
            if let source {
                if source.supportedPolarPatterns?.contains(.stereo) == true {
                    try source.setPreferredPolarPattern(.stereo)
                    stereo = true
                }
                try port.setPreferredDataSource(source)
            }
        } catch {
            NSLog("AudioNet: the microphone could not be set up: %@", error.localizedDescription)
        }
        // Two channels once the stereo pattern is in place (the maximum
        // follows the route).
        var end = Date().addingTimeInterval(1.0)
        while stereo, Date() < end, session.maximumInputNumberOfChannels < 2 { Thread.sleep(forTimeInterval: 0.02) }
        try? session.setPreferredInputNumberOfChannels(min(2, session.maximumInputNumberOfChannels))
        // Mono sources (a wired headset, a microphone without a stereo
        // pattern) stay at one channel: wait for two only when asked for.
        let channels = stereo ? min(2, session.maximumInputNumberOfChannels) : 1
        end = Date().addingTimeInterval(1.5)
        while Date() < end,
              session.currentRoute.inputs.first?.uid != port.uid || session.inputNumberOfChannels < channels {
            Thread.sleep(forTimeInterval: 0.02)
        }
        let used = session.currentRoute.inputs.first
        let usedSource = used?.selectedDataSource
        let text = "Microphone in use: \(used?.portName ?? "none")"
            + (usedSource.map { ", \($0.dataSourceName)" } ?? "")
            + (usedSource?.selectedPolarPattern == .stereo ? ", stereo pattern" : "")
            + ", \(session.inputNumberOfChannels) channel\(session.inputNumberOfChannels == 1 ? "" : "s")"
            + " at \(Int(session.sampleRate)) Hz."
        print("AudioNet: \(text)")
        NotificationCenter.default.post(name: microphoneInUse, object: text)
    }

    /// Posted (on the engine's thread) with a line saying which microphone
    /// records and how, or what went wrong with the audio session, for the
    /// status log.
    static let microphoneInUse = Notification.Name("AudioNetMicrophoneInUse")

    /// A problem, in words, for the status log (and the console).
    private static func report(_ text: String) {
        print("AudioNet: \(text)")
        NotificationCenter.default.post(name: microphoneInUse, object: text)
    }

    /// Bluetooth inputs (HFP, LE audio), which AudioNet never uses.
    private static func isBluetooth(_ p: AVAudioSessionPortDescription) -> Bool {
        p.portType == .bluetoothHFP || p.portType == .bluetoothLE
    }

    private static func parse(_ id: String) -> (port: String, source: String?) {
        guard let r = id.range(of: "::", options: .backwards) else { return (id, nil) }
        let source = String(id[r.upperBound...])
        return (String(id[..<r.lowerBound]), source == "default" ? nil : source)
    }

    /// The microphones this iPhone has now, grouped by input as UniMic lists
    /// them; never Bluetooth ones. iOS lists
    /// inputs for a play-and-record session: with no stream running the
    /// inactive session is set up that way for a moment to ask; while
    /// recording the session already is; while only playing, `nil` (keep the
    /// last list; changing a playing session would interrupt it).
    static func microphones() -> (groups: [MicrophoneGroup], current: String?)? {
        state.withLock { s -> (groups: [MicrophoneGroup], current: String?)? in
            guard s.streams == 0 || s.record else { return nil }
            if s.streams == 0 {
                try? session.setCategory(.playAndRecord, mode: .default, options: [.mixWithOthers, .allowBluetoothA2DP])
            }
            let ports = session.availableInputs ?? []
            let now = session.currentRoute.inputs.first
            let current = now.map { p in
                "\(p.uid)::\((p.selectedDataSource ?? p.preferredDataSource).map { "\($0.dataSourceID)" } ?? "default")"
            }
            if s.streams == 0 { configure(s) }
            let groups = ports.filter { !isBluetooth($0) }.map { port -> MicrophoneGroup in
                let group = port.portName
                let sources = port.dataSources ?? []
                let mics = sources.isEmpty
                    ? [Microphone(id: "\(port.uid)::default", name: "Default", fullName: group, stereo: false)]
                    : sources.map { d in
                        Microphone(id: "\(port.uid)::\(d.dataSourceID)", name: d.dataSourceName,
                                   fullName: "\(group), \(d.dataSourceName)",
                                   stereo: d.supportedPolarPatterns?.contains(.stereo) == true)
                    }
                return MicrophoneGroup(id: port.uid, name: group, microphones: mics)
            }
            return (groups, current)
        }
    }

    /// Where sound plays now, in words ("Speaker", "AirPods Pro").
    static var currentOutput: String {
        session.currentRoute.outputs.first?.portName ?? "Speaker"
    }
}

/// Hands the engine's stream counts to the audio session.
final class AudioUse: AudioUseListener {
    func audioInUse(streams: UInt32, microphones: UInt32) {
        AudioSession.streamsChanged(streams: streams, microphones: microphones)
    }
}
