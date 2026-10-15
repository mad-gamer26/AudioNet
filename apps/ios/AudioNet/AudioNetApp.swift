import SwiftUI
import UIKit

@main
struct AudioNetApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var model = AppModel()

    init() {
        // Sliders' unfilled track in a darker grey: the system's default is
        // too pale to see against a white row (WCAG asks 3:1 for controls).
        UISlider.appearance().maximumTrackTintColor = .systemGray
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
                .environmentObject(Notifications.shared)
        }
    }
}
