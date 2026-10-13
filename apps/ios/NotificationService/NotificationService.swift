import CryptoKit
import Foundation
import Security
import UserNotifications
import WidgetKit

/// Opens AudioNet's notifications: the server sealed the title (the account
/// name), the text ("Studio PC is online.") and what it is about (the
/// device's id and name, and the event) with this iPhone's key
/// (ChaCha20-Poly1305: nonce, ciphertext and tag, base64 in "e"); the push
/// gateway and Apple carried only that and a neutral placeholder.
///
/// It also keeps the widgets current (the device's new state goes into the
/// shared state), groups notifications by account, offers Listen for a
/// device that can be listened to, and delivers quietly during a Focus that
/// asks for it, or (if chosen in Settings) a device going offline as Time
/// Sensitive.
final class NotificationService: UNNotificationServiceExtension {
    /// The key the app keeps in the keychain group both share.
    private static func key() -> Data? {
        let group = Bundle.main.object(forInfoDictionaryKey: "AudioNetKeychainGroup") as? String ?? ""
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "AudioNet notifications",
            kSecAttrAccount as String: "notificationKey",
            kSecAttrAccessGroup as String: group,
            kSecReturnData as String: true,
        ]
        var found: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &found) == errSecSuccess else { return nil }
        return found as? Data
    }

    override func didReceive(_ request: UNNotificationRequest,
                             withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void) {
        guard let content = request.content.mutableCopy() as? UNMutableNotificationContent else {
            contentHandler(request.content)
            return
        }
        if let sealed = (content.userInfo["e"] as? String).flatMap({ Data(base64Encoded: $0) }),
           let key = Self.key(),
           let box = try? ChaChaPoly.SealedBox(combined: sealed),
           let plain = try? ChaChaPoly.open(box, using: SymmetricKey(data: key)),
           let text = try? JSONSerialization.jsonObject(with: plain) as? [String: String] {
            if let title = text["title"] {
                content.title = title
                // One group per account.
                content.threadIdentifier = title
            }
            if let body = text["body"] { content.body = body }
            if let nodeId = text["node_id"], let event = text["event"] {
                apply(event, to: nodeId, content: content)
            }
        }
        contentHandler(content)
    }

    /// What the notification says about a device: its state in the shared
    /// state (and the widgets), Listen, and how it is delivered.
    private func apply(_ event: String, to nodeId: String, content: UNMutableNotificationContent) {
        content.userInfo["node_id"] = nodeId
        var listenable = false
        var settings = SharedState()
        SharedState.update { state in
            settings = state
            guard let i = state.devices.firstIndex(where: { $0.nodeId == nodeId }) else { return }
            switch event {
            case "online": state.devices[i].online = true
            case "offline": state.devices[i].online = false
            case "sharing": state.devices[i].sharing = true
            case "not_sharing": state.devices[i].sharing = false
            default: break
            }
            listenable = state.devices[i].listenable
        }
        WidgetCenter.shared.reloadAllTimelines()
        if listenable && (event == "online" || event == "sharing") {
            content.categoryIdentifier = "audionet.listenable"
        }
        if settings.quietNotifications {
            content.interruptionLevel = .passive
        } else if event == "offline" && settings.timeSensitiveOffline {
            content.interruptionLevel = .timeSensitive
        }
    }
}
