import Foundation
import UIKit

/// The app's side of the share extension: it tells the extension which
/// notes and folders there are, and adds what was shared from other apps
/// (left in the App Group's inbox) to its notes.
struct ShareImport {
    let core: NotesCore
    let inbox: ShareInbox

    /// The notes and folders for the extension's lists.
    func catalog() -> ShareCatalog {
        let overview = core.overview()
        var folders: [ShareCatalog.Folder] = []
        var folderNames: [String: String] = [:]
        for a in overview.accounts {
            for f in a.folders {
                folders.append(.init(id: f.id, name: f.name, account: a.name, depth: Int(f.depth)))
                folderNames[f.id] = f.name
            }
        }
        // Recently edited first, as the app's own lists sort them.
        let notes = core.sections(place: .allNotes, search: "")
            .flatMap(\.notes)
            .filter { !$0.locked && !$0.deleted }
            .prefix(2000)
            .map { n in
                ShareCatalog.Note(id: n.id, title: n.title, folder: n.folder ?? "", time: n.time)
            }
        return ShareCatalog(notes: Array(notes), folders: folders, defaultFolder: core.defaultFolder())
    }

    func writeCatalog() {
        inbox.writeCatalog(catalog())
    }

    /// Adds every drop waiting to its note. Returns the notes added to, by
    /// id (in order, once each).
    @discardableResult
    func importPending() -> [String] {
        inbox.cleanStaging()
        var ids: [String] = []
        for (drop, folder) in inbox.pending() {
            if let id = add(drop, from: folder), !ids.contains(id) {
                ids.append(id)
            }
            inbox.remove(folder)
        }
        return ids
    }

    /// Adds one drop: to its note if that's still there and open, else to a
    /// new note. Returns the note's id.
    func add(_ drop: SharedDrop, from folder: URL) -> String? {
        if drop.isEmpty { return nil }
        let editor: NoteEditor
        var isNew = false
        if let id = drop.noteId, core.exists(id: id), !core.isDeleted(id: id), !core.isClosed(id: id),
           let e = core.openEditor(id: id) {
            editor = e
        } else {
            let known = Set(core.overview().accounts.flatMap { $0.folders.map(\.id) })
            let target = drop.folderId.flatMap { known.contains($0) ? $0 : nil } ?? core.defaultFolder()
            let n = core.newNote(place: .folder(id: target))
            editor = core.newNoteEditor(id: n.id)
            isNew = true
        }
        let writer = NoteAppender(editor: editor, startsEmpty: isNew)
        let id = editor.noteId()

        // What was typed, then the links, then pictures and files, as
        // Apple Notes puts them.
        let typed = drop.text
            .replacingOccurrences(of: "\r\n", with: "\n")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        var lines = typed.isEmpty ? [] : typed.components(separatedBy: "\n")
        // A new note with nothing typed is named after the page shared.
        if isNew, lines.isEmpty, let title = drop.links.first?.title?.trimmingCharacters(in: .whitespacesAndNewlines), !title.isEmpty {
            lines.append(title)
        }
        lines.append(contentsOf: drop.links.map(\.url))
        for line in lines {
            writer.add(line)
        }
        for f in drop.files {
            let url = folder.appendingPathComponent(f.path)
            guard let data = try? Data(contentsOf: url) else { continue }
            if f.isImage, UIImage(data: data) != nil,
               let name = try? core.addNoteImage(id: id, data: NoteImages.portable(data)) {
                writer.insert(.insertImage(name: name))
            } else if let reference = try? core.addNoteFile(id: id, data: data, fileName: f.name) {
                writer.insert(.insertFile(reference: reference, label: f.name))
            }
        }
        _ = core.saveEditor(editor: editor)
        return id
    }
}

/// Writes at the end of a note, a paragraph at a time.
final class NoteAppender {
    let editor: NoteEditor
    /// Nothing written into the note's first paragraph yet (a new note's
    /// title).
    private var atStart: Bool

    init(editor: NoteEditor, startsEmpty: Bool) {
        self.editor = editor
        atStart = startsEmpty || editor.plainText().isEmpty
    }

    private var end: UInt32 { UInt32((editor.plainText() as NSString).length) }

    private func toEnd() {
        let e = end
        editor.select(start: e, end: e)
    }

    /// A paragraph of text after what's there: in a new note the first one
    /// is the title, the others (and all those added to a note) are body
    /// text, whatever paragraph (a list, a checklist) came before.
    func add(_ text: String) {
        if atStart {
            atStart = false
        } else {
            newParagraph()
        }
        if !text.isEmpty {
            let e = end
            _ = editor.replace(start: e, end: e, text: text)
        }
    }

    /// A picture or file, on a line of its own after what's there.
    func insert(_ command: EditorCommand) {
        // (In an empty note it takes the first line.)
        atStart = false
        toEnd()
        editor.command(command: command)
    }

    private func newParagraph() {
        let e = end
        _ = editor.replace(start: e, end: e, text: "\n")
        toEnd()
        editor.command(command: .setStyle(style: .body))
    }
}

extension NotesStore {
    /// Adds what was shared from other apps, and says where it went.
    func importShared() {
        guard startupError == nil, let inbox = ShareInbox.shared else { return }
        saveEditor()
        let share = ShareImport(core: core, inbox: inbox)
        let ids = share.importPending()
        if !ids.isEmpty {
            changed()
            // The note on screen shows what was added to it.
            for f in onSyncChange.values { f() }
            if ids.count == 1 {
                let title = core.note(id: ids[0], place: .allNotes)?.title ?? ""
                show(title.isEmpty ? "Added to a new note." : "Added to “\(title)”.")
            } else {
                show("Added to \(ids.count) notes.")
            }
        }
    }

    /// Tells the share extension which notes and folders there are.
    func writeShareCatalog() {
        guard startupError == nil, let inbox = ShareInbox.shared else { return }
        ShareImport(core: core, inbox: inbox).writeCatalog()
    }
}
