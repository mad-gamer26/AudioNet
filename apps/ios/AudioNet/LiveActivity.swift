import ActivityKit
import Foundation

/// Runs AudioNet's Live Activity: started with the first stream this iPhone
/// starts, kept current (what streams, its state, muted), and ended with the
/// last one. Stopped on purpose, it goes at once; lost, it stays a few
/// minutes saying why, with an alert (sound or vibration, as the iPhone is
/// set). The person can turn Live Activities off for AudioNet in Settings.
@MainActor
final class StreamsActivityController {
    private var activity: Activity<StreamsActivity>?
    private var last: StreamsActivity.ContentState?
    private var since = Date()

    /// Ends activities left from an earlier run (AudioNet was closed while
    /// streaming).
    func endLeftovers() {
        for leftover in Activity<StreamsActivity>.activities {
            let a = Shared(leftover)
            Task.detached { await a.value.end(nil, dismissalPolicy: .immediate) }
        }
    }

    func update(streams: [StreamRow], others: Int) {
        guard !streams.isEmpty else {
            // Gone without a word from the engine (AudioNet went offline).
            if activity != nil { end(reason: "AudioNet went offline.", alert: true) }
            return
        }
        if activity == nil { since = Date() }
        let state = content(streams, others: others)
        guard state != last else { return }
        last = state
        if let activity {
            let a = Shared(activity)
            Task.detached { await a.value.update(ActivityContent(state: state, staleDate: nil)) }
        } else if ActivityAuthorizationInfo().areActivitiesEnabled {
            activity = try? Activity.request(attributes: StreamsActivity(),
                                             content: ActivityContent(state: state, staleDate: nil))
        }
    }

    /// A stream ended; with none left, the activity ends.
    func streamEnded(_ row: StreamRow, reason: String, byUser: Bool, remaining: Int) {
        guard remaining == 0, activity != nil else { return }
        if byUser {
            end(reason: nil, alert: false)
        } else {
            let why = reason.trimmingCharacters(in: .whitespaces)
            end(reason: "\(row.deviceName) stopped" + (why.isEmpty ? "." : ": \(why)"), alert: true)
        }
    }

    private func end(reason: String?, alert: Bool) {
        guard let activity else { return }
        self.activity = nil
        let previous = last
        last = nil
        let a = Shared(activity)
        guard let reason, var state = previous else {
            Task.detached { await a.value.end(nil, dismissalPolicy: .immediate) }
            return
        }
        state.ended = true
        state.count = 0
        state.muted = false
        state.detail = reason
        let content = ActivityContent(state: state, staleDate: nil)
        Task.detached {
            if alert {
                await a.value.update(content, alertConfiguration: AlertConfiguration(
                    title: "AudioNet", body: "\(reason)", sound: .default))
            }
            await a.value.end(content, dismissalPolicy: .after(Date().addingTimeInterval(5 * 60)))
        }
    }

    private func content(_ streams: [StreamRow], others: Int) -> StreamsActivity.ContentState {
        let muted = streams.allSatisfy(\.muted)
        var detail: String
        let headline: String
        if streams.count == 1, let row = streams.first {
            headline = row.listening ? "Listening to \(row.deviceName)" : "Sending to \(row.deviceName)"
            detail = stateWords(row.state)
        } else {
            headline = "\(streams.count) streams"
            let names = ListFormatter.localizedString(byJoining: streams.map(\.deviceName))
            let connected = streams.allSatisfy { $0.state == "connected" }
            detail = connected ? "With \(names), connected" : "With \(names)"
        }
        if muted { detail += ", muted" }
        if others == 1 { detail += ". Also 1 stream another device started" }
        if others > 1 { detail += ". Also \(others) streams other devices started" }
        return StreamsActivity.ContentState(
            headline: headline, detail: detail + ".",
            shortName: streams.count == 1 ? streams[0].deviceName : "\(streams.count) streams",
            listening: streams.contains(where: \.listening), muted: muted, count: streams.count,
            ended: false, since: since)
    }

    /// "connected" → "Connected"; the engine's other states as they are.
    private func stateWords(_ state: String) -> String {
        state.prefix(1).uppercased() + state.dropFirst()
    }
}

/// An activity handed to a background task. ActivityKit's activities may be
/// updated and ended from any thread; the type is not marked Sendable.
private struct Shared: @unchecked Sendable {
    let value: Activity<StreamsActivity>
    init(_ value: Activity<StreamsActivity>) { self.value = value }
}
