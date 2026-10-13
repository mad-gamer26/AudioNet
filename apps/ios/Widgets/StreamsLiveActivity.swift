import ActivityKit
import AppIntents
import SwiftUI
import WidgetKit

/// How AudioNet's Live Activity looks: on the Lock Screen, and in the
/// Dynamic Island (compact, minimal and expanded). Everything it says is
/// text VoiceOver reads; the icons only repeat it. Mute and Stop run in the
/// app (where the streams are).
struct StreamsLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: StreamsActivity.self) { context in
            LockScreenCard(state: context.state)
                .padding()
                .activityBackgroundTint(nil)
        } dynamicIsland: { context in
            let state = context.state
            return DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    StreamIcon(state: state)
                        .font(.title2)
                }
                DynamicIslandExpandedRegion(.center) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(state.headline).font(.headline).lineLimit(1)
                        Text(state.detail).font(.caption).lineLimit(2)
                    }
                    .accessibilityElement(children: .combine)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    if !state.ended {
                        Elapsed(since: state.since)
                            .font(.caption)
                    }
                }
                DynamicIslandExpandedRegion(.bottom) {
                    if !state.ended {
                        StreamButtons(state: state)
                    }
                }
            } compactLeading: {
                StreamIcon(state: state)
            } compactTrailing: {
                Text(state.shortName)
                    .lineLimit(1)
                    .frame(maxWidth: 80)
                    .accessibilityLabel(state.headline)
            } minimal: {
                StreamIcon(state: state)
            }
            .keylineTint(.blue)
        }
    }
}

/// The Lock Screen card: what streams, its state, how long, Mute and Stop.
private struct LockScreenCard: View {
    let state: StreamsActivity.ContentState

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .top) {
                StreamIcon(state: state)
                    .font(.title2)
                VStack(alignment: .leading, spacing: 2) {
                    Text(state.headline).font(.headline)
                    Text(state.detail).font(.subheadline)
                }
                .accessibilityElement(children: .combine)
                Spacer()
                if !state.ended {
                    Elapsed(since: state.since)
                        .font(.subheadline)
                }
            }
            if !state.ended {
                StreamButtons(state: state)
            }
        }
    }
}

/// Headphones while listening, a microphone while sending, a stop sign
/// once ended. Its name says the same in words.
private struct StreamIcon: View {
    let state: StreamsActivity.ContentState

    var body: some View {
        Image(systemName: state.ended ? "stop.circle"
              : state.muted ? "speaker.slash.fill"
              : state.listening ? "headphones" : "mic.fill")
            .accessibilityLabel(state.ended ? "Ended" : state.muted ? "Muted"
                                : state.listening ? "Listening" : "Sending")
    }
}

/// How long it has streamed, counted by iOS itself (no updates needed).
private struct Elapsed: View {
    let since: Date

    var body: some View {
        Text(timerInterval: since...Date.distantFuture, countsDown: false)
            .monospacedDigit()
            .multilineTextAlignment(.trailing)
            .frame(maxWidth: 70)
    }
}

/// Mute (or Unmute) and Stop, for every stream started on this iPhone.
private struct StreamButtons: View {
    let state: StreamsActivity.ContentState

    var body: some View {
        HStack {
            Button(intent: SetMutedIntent(value: !state.muted)) {
                Label(state.muted ? "Unmute" : "Mute",
                      systemImage: state.muted ? "speaker.wave.2.fill" : "speaker.slash.fill")
                    .frame(maxWidth: .infinity)
            }
            Button(intent: StopStreamsIntent()) {
                Label(state.count > 1 ? "Stop All" : "Stop", systemImage: "stop.fill")
                    .frame(maxWidth: .infinity)
            }
            .tint(.red)
        }
        .buttonStyle(.bordered)
    }
}
