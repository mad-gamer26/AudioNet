import ActivityKit
import Foundation

/// AudioNet's Live Activity: one for everything this iPhone streams (one
/// card per stream would crowd the Lock Screen and the Dynamic Island).
/// The app starts it with the first stream, keeps it current, and ends it
/// with the last one; the widget extension draws it.
struct StreamsActivity: ActivityAttributes {
    struct ContentState: Codable, Hashable, Sendable {
        /// The main line: "Listening to Studio PC", "Sending the microphone
        /// to Living Room PC", "3 streams".
        var headline: String
        /// The second line, in words: what plays where, its state
        /// ("Connected", "Reconnecting"), muted, streams other devices
        /// started, or why it ended.
        var detail: String
        /// A short name for the compact Dynamic Island ("Studio PC").
        var shortName: String
        /// Listening (headphones) or sending (microphone).
        var listening: Bool
        /// Every stream started here is muted.
        var muted: Bool
        /// Streams started here (Mute and Stop act on them).
        var count: Int
        /// Nothing streams any more; `detail` says why.
        var ended: Bool
        /// When the first stream started (for the elapsed time).
        var since: Date
    }
}
