import SwiftUI
import WidgetKit

/// Everything the widget extension offers: the Live Activity, the widgets,
/// and (iOS 18 and later) the Control Center controls.
@main
struct AudioNetWidgets: WidgetBundle {
    var body: some Widget {
        StreamsLiveActivity()
        DevicesWidget()
        DeviceWidget()
        if #available(iOS 18.0, *) {
            StopStreamsControl()
            MuteControl()
            ListenControl()
        }
    }
}
