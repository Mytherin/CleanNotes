import Foundation
import LocalAuthentication
import Security

/// Face ID (or Touch ID) for locked notes, as in Apple Notes: when it's
/// turned on in Settings, the notes password is kept in the keychain, on
/// this iPhone only, where it can only be read after Face ID recognizes the
/// owner. Enrolling another face makes the keychain drop it, and the password
/// has to be entered again. The password is never kept anywhere else.
enum Biometrics {
    private static let service = "com.cleannotes.locked-notes-password"
    private static let account = "password"
    private static let enabledKey = "UseBiometricsForLockedNotes"

    /// Turned on in Settings (and the password is in the keychain).
    static var enabled: Bool {
        get { UserDefaults.standard.bool(forKey: enabledKey) }
        set { UserDefaults.standard.set(newValue, forKey: enabledKey) }
    }

    /// The device has Face ID or Touch ID set up.
    static var available: Bool {
        LAContext().canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: nil)
    }

    /// Enabled and usable now.
    static var ready: Bool { enabled && available }

    /// "Face ID", "Touch ID" or "Optic ID".
    static var name: String {
        let context = LAContext()
        _ = context.canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: nil)
        switch context.biometryType {
        case .touchID: return "Touch ID"
        case .opticID: return "Optic ID"
        default: return "Face ID"
        }
    }

    /// Keeps the password (replacing one kept before) and turns Face ID on.
    @discardableResult
    static func turnOn(password: String) -> Bool {
        remove()
        var error: Unmanaged<CFError>?
        guard let access = SecAccessControlCreateWithFlags(
            nil,
            kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly,
            .biometryCurrentSet,
            &error
        ) else {
            return false
        }
        let item: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecAttrAccessControl as String: access,
            kSecValueData as String: Data(password.utf8),
        ]
        let ok = SecItemAdd(item as CFDictionary, nil) == errSecSuccess
        enabled = ok
        return ok
    }

    /// Forgets the password and turns Face ID off.
    static func turnOff() {
        remove()
        enabled = false
    }

    /// The password changed: the kept one follows (if Face ID is on).
    static func passwordChanged(to password: String) {
        if enabled {
            turnOn(password: password)
        }
    }

    /// Asks for Face ID and gives the kept password (on the main thread),
    /// or nil: cancelled, not recognized, or the password is gone (a new
    /// face was enrolled), in which case Face ID turns off.
    static func password(reason: String, completion: @escaping (String?) -> Void) {
        guard ready else {
            completion(nil)
            return
        }
        // Reading the item shows the Face ID prompt and waits for it: not
        // on the main thread.
        DispatchQueue.global(qos: .userInitiated).async {
            let context = LAContext()
            context.localizedReason = reason
            context.localizedFallbackTitle = "Enter Password"
            let query: [String: Any] = [
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: Biometrics.service,
                kSecAttrAccount as String: Biometrics.account,
                kSecReturnData as String: true,
                kSecMatchLimit as String: kSecMatchLimitOne,
                kSecUseAuthenticationContext as String: context,
            ]
            var result: CFTypeRef?
            let status = SecItemCopyMatching(query as CFDictionary, &result)
            let password = status == errSecSuccess ? (result as? Data).flatMap { String(data: $0, encoding: .utf8) } : nil
            DispatchQueue.main.async {
                if status == errSecItemNotFound {
                    Biometrics.enabled = false
                }
                completion(password)
            }
        }
    }

    private static func remove() {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        SecItemDelete(query as CFDictionary)
    }
}
