import Foundation
import LocalAuthentication
import Security

/// A password kept for Face ID (or Touch ID), as in Apple Notes: when it's
/// turned on, the password is kept in the keychain, on this iPhone only,
/// where it can only be read after Face ID recognizes the owner. Enrolling
/// another face makes the keychain drop it, and the password has to be
/// entered again. The password is never kept anywhere else.
///
/// One for the locked notes' password (`Biometrics`), one for
/// CleanPassword's master password (`VaultFaceID`).
final class KeptPassword {
    static let lockedNotes = KeptPassword(service: "com.cleannotes.locked-notes-password", enabledKey: "UseBiometricsForLockedNotes")
    static let vault = KeptPassword(service: "com.cleannotes.cleanpassword-master-password", enabledKey: "UseBiometricsForCleanPassword")

    private let service: String
    private let account = "password"
    private let enabledKey: String

    private init(service: String, enabledKey: String) {
        self.service = service
        self.enabledKey = enabledKey
    }

    /// Turned on (and the password is in the keychain).
    var enabled: Bool {
        get { UserDefaults.standard.bool(forKey: enabledKey) }
        set { UserDefaults.standard.set(newValue, forKey: enabledKey) }
    }

    /// Enabled and usable now.
    var ready: Bool { enabled && Biometrics.available }

    /// Keeps the password (replacing one kept before) and turns Face ID on.
    @discardableResult
    func turnOn(password: String) -> Bool {
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
    func turnOff() {
        remove()
        enabled = false
    }

    /// The password changed: the kept one follows (if Face ID is on).
    func passwordChanged(to password: String) {
        if enabled {
            turnOn(password: password)
        }
    }

    /// Asks for Face ID and gives the kept password (on the main thread),
    /// or nil: cancelled, not recognized, or the password is gone (a new
    /// face was enrolled), in which case Face ID turns off.
    func password(reason: String, completion: @escaping (String?) -> Void) {
        guard ready else {
            completion(nil)
            return
        }
        let service = service, account = account
        // Reading the item shows the Face ID prompt and waits for it: not
        // on the main thread.
        DispatchQueue.global(qos: .userInitiated).async {
            let context = LAContext()
            context.localizedReason = reason
            context.localizedFallbackTitle = "Enter Password"
            let query: [String: Any] = [
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: service,
                kSecAttrAccount as String: account,
                kSecReturnData as String: true,
                kSecMatchLimit as String: kSecMatchLimitOne,
                kSecUseAuthenticationContext as String: context,
            ]
            var result: CFTypeRef?
            let status = SecItemCopyMatching(query as CFDictionary, &result)
            let password = status == errSecSuccess ? (result as? Data).flatMap { String(data: $0, encoding: .utf8) } : nil
            DispatchQueue.main.async {
                if status == errSecItemNotFound {
                    self.enabled = false
                }
                completion(password)
            }
        }
    }

    private func remove() {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        SecItemDelete(query as CFDictionary)
    }
}

/// Face ID (or Touch ID) for locked notes: the notes password, kept for it
/// when it's turned on in Settings.
enum Biometrics {
    /// The device has Face ID or Touch ID set up.
    static var available: Bool {
        LAContext().canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: nil)
    }

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

    static var enabled: Bool {
        get { KeptPassword.lockedNotes.enabled }
        set { KeptPassword.lockedNotes.enabled = newValue }
    }

    static var ready: Bool { KeptPassword.lockedNotes.ready }

    @discardableResult
    static func turnOn(password: String) -> Bool { KeptPassword.lockedNotes.turnOn(password: password) }

    static func turnOff() { KeptPassword.lockedNotes.turnOff() }

    static func passwordChanged(to password: String) { KeptPassword.lockedNotes.passwordChanged(to: password) }

    static func password(reason: String, completion: @escaping (String?) -> Void) {
        KeptPassword.lockedNotes.password(reason: reason, completion: completion)
    }
}

/// Face ID (or Touch ID) for CleanPassword: its master password, kept for it
/// once it has unlocked the vault (so a mistyped one is never kept). Offered
/// after the first unlock with the master password, and turned on or off in
/// Settings; when the master password changes on another device, the next
/// one typed that unlocks the vault replaces it.
enum VaultFaceID {
    private static let wantedKey = "CleanPasswordFaceIDWanted"
    private static let offeredKey = "CleanPasswordFaceIDOffered"

    static var kept: KeptPassword { .vault }

    /// Turned on in Settings while there was no master password to keep:
    /// the next one that unlocks the vault is kept.
    static var wanted: Bool {
        get { UserDefaults.standard.bool(forKey: wantedKey) }
        set { UserDefaults.standard.set(newValue, forKey: wantedKey) }
    }

    /// Asked once whether to use Face ID (after the first unlock).
    static var offered: Bool {
        get { UserDefaults.standard.bool(forKey: offeredKey) }
        set { UserDefaults.standard.set(newValue, forKey: offeredKey) }
    }

    /// On, or waiting for the master password to be typed.
    static var on: Bool { kept.enabled || wanted }

    /// The master password typed unlocked the vault. Returns whether to ask
    /// if Face ID should keep it (the first time).
    static func unlocked(with password: String) -> Bool {
        guard Biometrics.available else { return false }
        if on {
            wanted = !kept.turnOn(password: password)
            return false
        }
        if offered { return false }
        offered = true
        return true
    }

    static func turnOff() {
        kept.turnOff()
        wanted = false
    }
}
