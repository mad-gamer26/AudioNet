import SwiftUI

/// A line of text that changes often (a stream's measurements, every two
/// seconds), in an object of its own: only the view showing it redraws.
/// Kept in the app model, a change would redraw the whole screen each time,
/// moving VoiceOver's place and closing open menus.
@MainActor
final class LiveText: ObservableObject {
    @Published var text: String

    init(_ text: String) {
        self.text = text
    }
}

/// Shows a ``LiveText``; redraws alone when it changes.
struct LiveTextRow: View {
    @ObservedObject var live: LiveText

    var body: some View {
        Text(live.text)
    }
}
