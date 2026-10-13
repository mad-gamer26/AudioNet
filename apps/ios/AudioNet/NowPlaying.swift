import Foundation
import MediaPlayer
import SwiftUI

/// "Lock Screen and AirPods Controls" (Settings, off by default): while a
/// stream plays, AudioNet appears in the Lock Screen and Control Center
/// player (and the Apple Watch's Now Playing); play and pause, from there or
/// from AirPods, unmute and mute every stream, and stop stops them.
///
/// Off by default because iOS shows only an app that does not mix with
/// others there: with it on, other apps' audio pauses while AudioNet plays.
/// AudioNet otherwise never stops or lowers a podcast or music. The Live
/// Activity offers Mute and Stop either way.
@MainActor
final class NowPlayingController {
    weak var model: AppModel?

    @AppStorage("lockScreenControls", store: AccountStore.settings)
    private var enabledSetting = false
    private var commandsReady = false
    private var showing = false

    var enabled: Bool {
        get { enabledSetting }
        set {
            enabledSetting = newValue
            AudioSession.setExclusive(newValue)
            if let model { update(streams: model.streams) }
        }
    }

    init() {
        AudioSession.setExclusive(enabledSetting)
    }

    func update(streams: [StreamRow]) {
        let center = MPNowPlayingInfoCenter.default()
        guard enabledSetting, !streams.isEmpty else {
            if showing {
                center.nowPlayingInfo = nil
                center.playbackState = .stopped
                showing = false
            }
            return
        }
        setUpCommands()
        let muted = streams.allSatisfy(\.muted)
        let title = streams.count == 1
            ? (streams[0].listening ? "Listening to \(streams[0].deviceName)" : "Sending to \(streams[0].deviceName)")
            : "\(streams.count) AudioNet streams"
        center.nowPlayingInfo = [
            MPMediaItemPropertyTitle: title,
            MPMediaItemPropertyArtist: muted ? "AudioNet, muted" : "AudioNet",
            MPNowPlayingInfoPropertyIsLiveStream: true,
            MPNowPlayingInfoPropertyPlaybackRate: muted ? 0.0 : 1.0,
        ]
        center.playbackState = muted ? .paused : .playing
        showing = true
    }

    /// Play and pause unmute and mute; stop stops; nothing to seek or skip.
    private func setUpCommands() {
        guard !commandsReady else { return }
        commandsReady = true
        // iOS calls these on the main thread.
        let c = MPRemoteCommandCenter.shared()
        c.playCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated { self?.mute(false) ?? .commandFailed }
        }
        c.pauseCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated { self?.mute(true) ?? .commandFailed }
        }
        c.togglePlayPauseCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, let model = self.model else { return .commandFailed }
                return self.mute(!model.streams.allSatisfy(\.muted))
            }
        }
        c.stopCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated {
                guard let model = self?.model, !model.streams.isEmpty else { return .noActionableNowPlayingItem }
                _ = model.stopAll()
                return .success
            }
        }
        for unused in [c.nextTrackCommand, c.previousTrackCommand, c.skipForwardCommand,
                       c.skipBackwardCommand, c.seekForwardCommand, c.seekBackwardCommand,
                       c.changePlaybackPositionCommand] {
            unused.isEnabled = false
        }
    }

    private func mute(_ muted: Bool) -> MPRemoteCommandHandlerStatus {
        guard let model, !model.streams.isEmpty else { return .noActionableNowPlayingItem }
        _ = model.setMuted(muted)
        return .success
    }
}
