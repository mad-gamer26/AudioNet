import SwiftUI
import UIKit

@main
struct AudioNetApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var model = AppModel.shared

    init() {
        // Sliders' unfilled track in a darker grey: the system's default is
        // too pale to see against a white row (WCAG asks 3:1 for controls).
        UISlider.appearance().maximumTrackTintColor = .systemGray
        // Before anything else: Siri, a widget or a control may have started
        // AudioNet in the background to do something.
        Integrations.shared.start(AppModel.shared)
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
                .environmentObject(Notifications.shared)
                // audionet://device/ID and audionet://listen/ID.
                .onOpenURL { model.openLink($0) }
                // Handoff: what the Mac was listening to continues here, and
                // what this iPhone listens to is offered to the Mac.
                .onContinueUserActivity(handoffType) { model.continueListening($0) }
                .userActivity(handoffType, isActive: model.handoffStream != nil) { activity in
                    model.describeHandoff(activity)
                }
        }
    }
}

/// Handoff of a stream being listened to, shared with the Mac app.
let handoffType = "org.audionet.listen"
