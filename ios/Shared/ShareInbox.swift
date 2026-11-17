import Foundation

// What the share extension and the app pass each other, in their shared
// App Group folder. The extension never opens the notes library itself (the
// app may be holding it, and syncing it): it leaves what was shared in an
// inbox, and the app adds it to the note the next time it comes to the
// foreground. The app, in turn, leaves a list of its notes and folders there
// for the extension's "Choose Note" and "Folder" screens.
//
// This file is compiled into both the app and the extension.

/// The App Group both the app and the share extension belong to.
enum AppGroup {
    static let defaultIdentifier = "group.com.cleannotes.CleanNotes"

    /// The group's identifier. Signing tools that give the app a group of
    /// their own (AltStore) list it under `ALTAppGroups` in the app's
    /// Info.plist; the extension looks in the app that contains it.
    static var identifier: String {
        let bundles = [Bundle.main, containingApp].compactMap { $0 }
        for b in bundles {
            if let groups = b.object(forInfoDictionaryKey: "ALTAppGroups") as? [String], let first = groups.first {
                return first
            }
        }
        return defaultIdentifier
    }

    /// The app, seen from its extension (CleanNotes.app/PlugIns/x.appex).
    private static var containingApp: Bundle? {
        let url = Bundle.main.bundleURL
        guard url.pathExtension == "appex" else { return nil }
        return Bundle(url: url.deletingLastPathComponent().deletingLastPathComponent())
    }

    /// The shared folder: nil if the app was signed without the group.
    static var container: URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: identifier)
    }
}

/// The app's notes and folders, as the share extension lists them.
struct ShareCatalog: Codable, Equatable {
    struct Note: Codable, Equatable, Identifiable {
        var id: String
        var title: String
        /// Its folder's name.
        var folder: String
        /// "Yesterday", "11:32", … as the notes list shows it.
        var time: String
    }

    struct Folder: Codable, Equatable, Identifiable {
        var id: String
        var name: String
        var account: String
        /// How deep it's nested in its account.
        var depth: Int
    }

    /// Most recent first, as in the app's All Notes; locked and deleted
    /// notes are left out (nothing can be added to them from outside).
    var notes: [Note]
    /// Depth first, account by account.
    var folders: [Folder]
    /// Where new notes go when no folder is chosen.
    var defaultFolder: String
}

/// Something shared, waiting for the app.
struct SharedDrop: Codable, Equatable {
    struct Link: Codable, Equatable {
        var url: String
        /// The page's title, if the extension found it.
        var title: String?
    }

    struct File: Codable, Equatable {
        /// Its file in the drop's folder.
        var path: String
        /// The name it's shown with.
        var name: String
        /// Shown as a picture, not as a link to the file.
        var isImage: Bool
    }

    var created = Date()
    /// The note to add to; nil for a new note.
    var noteId: String?
    /// Where a new note goes; nil for the default folder.
    var folderId: String?
    /// What was typed (or the text shared).
    var text: String
    var links: [Link] = []
    var files: [File] = []

    /// Nothing to add.
    var isEmpty: Bool {
        text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && links.isEmpty && files.isEmpty
    }
}

/// The shared folder: the catalog, and the inbox of drops (a folder each,
/// with `drop.json` and the shared files).
struct ShareInbox {
    let root: URL

    /// The App Group's (nil if the app was signed without it).
    static var shared: ShareInbox? {
        AppGroup.container.map { ShareInbox(root: $0.appendingPathComponent("Share", isDirectory: true)) }
    }

    var inbox: URL { root.appendingPathComponent("Inbox", isDirectory: true) }
    private var staging: URL { root.appendingPathComponent("Staging", isDirectory: true) }
    private var catalogURL: URL { root.appendingPathComponent("catalog.json") }

    // ---- The catalog (written by the app) ----

    func writeCatalog(_ catalog: ShareCatalog) {
        guard let data = try? JSONEncoder().encode(catalog) else { return }
        try? FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try? data.write(to: catalogURL, options: .atomic)
    }

    func readCatalog() -> ShareCatalog? {
        guard let data = try? Data(contentsOf: catalogURL) else { return nil }
        return try? JSONDecoder().decode(ShareCatalog.self, from: data)
    }

    // ---- Drops (written by the extension) ----

    /// A folder to gather a drop's files in, before `post`.
    func newDraft() throws -> URL {
        let dir = staging.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    /// Puts the drop in the inbox, its files and all at once (the folder is
    /// moved in only when it's complete, so the app never sees half of it).
    func post(_ drop: SharedDrop, draft: URL) throws {
        let data = try JSONEncoder().encode(drop)
        try data.write(to: draft.appendingPathComponent("drop.json"), options: .atomic)
        try FileManager.default.createDirectory(at: inbox, withIntermediateDirectories: true)
        // Named by time, so drops are added in the order they were shared.
        let name = String(format: "%.6f-", drop.created.timeIntervalSince1970) + draft.lastPathComponent
        try FileManager.default.moveItem(at: draft, to: inbox.appendingPathComponent(name, isDirectory: true))
    }

    func discard(draft: URL) {
        try? FileManager.default.removeItem(at: draft)
    }

    // ---- Taking drops (the app) ----

    /// The drops waiting, oldest first, with their folders.
    func pending() -> [(drop: SharedDrop, folder: URL)] {
        let dirs = (try? FileManager.default.contentsOfDirectory(at: inbox, includingPropertiesForKeys: nil)) ?? []
        return dirs
            .sorted { $0.lastPathComponent < $1.lastPathComponent }
            .compactMap { dir in
                guard let data = try? Data(contentsOf: dir.appendingPathComponent("drop.json")),
                      let drop = try? JSONDecoder().decode(SharedDrop.self, from: data)
                else {
                    // Unreadable: nothing can be done with it.
                    try? FileManager.default.removeItem(at: dir)
                    return nil
                }
                return (drop, dir)
            }
    }

    func remove(_ folder: URL) {
        try? FileManager.default.removeItem(at: folder)
    }

    /// Drafts the extension left behind (it was closed while saving).
    func cleanStaging(olderThan age: TimeInterval = 3600) {
        let dirs = (try? FileManager.default.contentsOfDirectory(at: staging, includingPropertiesForKeys: [.creationDateKey])) ?? []
        for d in dirs {
            let created = (try? d.resourceValues(forKeys: [.creationDateKey]).creationDate) ?? .distantPast
            if Date().timeIntervalSince(created) > age {
                try? FileManager.default.removeItem(at: d)
            }
        }
    }
}

/// The extension's own memory, in the group's defaults: the folder last
/// chosen for new notes.
enum ShareMemory {
    private static var defaults: UserDefaults? { UserDefaults(suiteName: AppGroup.identifier) }

    static var lastFolder: String? {
        get { defaults?.string(forKey: "share.lastFolder") }
        set { defaults?.set(newValue, forKey: "share.lastFolder") }
    }
}
