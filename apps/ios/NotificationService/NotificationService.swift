import CryptoKit
import Foundation
import Security
import UserNotifications

/// Opens AudioNet's notifications: the server sealed the title (the account
/// name) and text ("Studio PC is online.") with this iPhone's key
/// (ChaCha20-Poly1305: nonce, ciphertext and tag, base64 in "e"); the push
/// gateway and Apple carried only that and a neutral placeholder.
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
            if let title = text["title"] { content.title = title }
            if let body = text["body"] { content.body = body }
        }
        contentHandler(content)
    }
}
