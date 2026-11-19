import SwiftUI
import UIKit

// Small things the screens share.

extension NoteItem: Identifiable {}
extension FolderItem: Identifiable {}
extension AccountItem: Identifiable {}
extension SmartFolderItem: Identifiable {}
extension VaultRow: Identifiable {}

/// Where the navigation stack can go.
enum Route: Hashable {
    /// A notes list.
    case list(Place)
    /// A note; `isNew` opens it with the keyboard up.
    case note(id: String, isNew: Bool)
    /// A CleanPassword list (All Passwords, Codes, a folder, Recently
    /// Deleted).
    case passwords(VaultPlace)
    /// A CleanPassword item.
    case password(id: String)
}

/// The message of an error from the core.
func message(_ error: Error) -> String {
    if case let CoreError.Failed(message) = error {
        return message
    }
    return error.localizedDescription
}

extension Color {
    /// Notes' yellow, from the asset catalog.
    static let notesAccent = Color("AccentColor")
}

extension UIColor {
    static let notesAccent = UIColor(named: "AccentColor") ?? .systemYellow
}

extension Place {
    var isDeleted: Bool {
        if case .deleted = self { return true }
        return false
    }

    var folderId: String? {
        if case let .folder(id) = self { return id }
        return nil
    }
}

/// "1 Note", "12 Notes".
func notesCount(_ n: Int) -> String {
    n == 1 ? "1 Note" : "\(n) Notes"
}
