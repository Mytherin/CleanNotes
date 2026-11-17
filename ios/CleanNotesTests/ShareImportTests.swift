import UIKit
import XCTest
@testable import CleanNotes

/// What the share extension leaves in the inbox, added to notes by the app
/// as Apple Notes adds what's shared to it.
final class ShareImportTests: XCTestCase {
    private var dir: URL!
    private var store: NotesStore!
    private var inbox: ShareInbox!

    override func setUpWithError() throws {
        dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        store = NotesStore(directory: dir.appendingPathComponent("notes"))
        XCTAssertNil(store.startupError)
        inbox = ShareInbox(root: dir.appendingPathComponent("share"))
    }

    override func tearDownWithError() throws {
        store = nil
        try? FileManager.default.removeItem(at: dir)
    }

    private var importer: ShareImport { ShareImport(core: store.core, inbox: inbox) }

    private func post(_ drop: SharedDrop, files: [String: Data] = [:]) throws {
        let draft = try inbox.newDraft()
        for (name, data) in files {
            try data.write(to: draft.appendingPathComponent(name))
        }
        try inbox.post(drop, draft: draft)
    }

    private func styles(_ id: String) -> [ParagraphStyle] {
        store.core.openEditor(id: id)!.state().paragraphs.map(\.style)
    }

    /// A note with `lines` typed in it (the first is its title).
    private func note(_ lines: [String]) -> String {
        let n = store.core.newNote(place: .folder(id: store.core.defaultFolder()))
        let e = store.core.newNoteEditor(id: n.id)
        let w = NoteAppender(editor: e, startsEmpty: true)
        lines.forEach(w.add)
        _ = store.core.saveEditor(editor: e)
        return n.id
    }

    func testTextSharedMakesANewNoteWithItAsTitleAndBody() throws {
        try post(SharedDrop(text: "Groceries\nmilk\neggs"))
        let ids = importer.importPending()
        XCTAssertEqual(ids.count, 1)
        let e = store.core.openEditor(id: ids[0])!
        XCTAssertEqual(e.plainText(), "Groceries\nmilk\neggs")
        XCTAssertEqual(styles(ids[0]), [.title, .body, .body])
        XCTAssertTrue(inbox.pending().isEmpty, "the drop is gone once added")
    }

    func testALinkWithNothingTypedIsNamedAfterThePage() throws {
        var drop = SharedDrop(text: "")
        drop.links = [.init(url: "https://example.com/a", title: "Example Page")]
        try post(drop)
        let id = importer.importPending()[0]
        XCTAssertEqual(store.core.openEditor(id: id)!.plainText(), "Example Page\nhttps://example.com/a")
    }

    func testSharingIntoANoteAddsBodyTextAtItsEnd() throws {
        let id = note(["Reading list", "first"])
        // The note ends in a checklist: what's added isn't a checklist item.
        let e = store.core.openEditor(id: id)!
        e.select(start: 20, end: 20)
        e.command(command: .toggleChecklist)
        _ = store.core.saveEditor(editor: e)

        var drop = SharedDrop(noteId: id, text: "Look at this")
        drop.links = [.init(url: "https://example.com/b", title: "B")]
        try post(drop)
        XCTAssertEqual(importer.importPending(), [id])
        XCTAssertEqual(store.core.openEditor(id: id)!.plainText(), "Reading list\nfirst\nLook at this\nhttps://example.com/b")
        XCTAssertEqual(styles(id), [.title, .checklist, .body, .body])
    }

    func testANoteGoneSinceGetsANewNoteInstead() throws {
        let id = note(["Old"])
        store.core.trashNotes(ids: [id], place: .allNotes)
        try post(SharedDrop(noteId: id, text: "Still kept"))
        let ids = importer.importPending()
        XCTAssertEqual(ids.count, 1)
        XCTAssertNotEqual(ids[0], id)
        XCTAssertEqual(store.core.openEditor(id: ids[0])!.plainText(), "Still kept")
    }

    func testPicturesAndFilesBecomeTheNotesOwn() throws {
        let png = UIGraphicsImageRenderer(size: CGSize(width: 4, height: 4)).pngData { ctx in
            UIColor.red.setFill()
            ctx.fill(CGRect(x: 0, y: 0, width: 4, height: 4))
        }
        var drop = SharedDrop(text: "Trip")
        drop.files = [
            .init(path: "photo.png", name: "photo.png", isImage: true),
            .init(path: "ticket.pdf", name: "ticket.pdf", isImage: false),
        ]
        try post(drop, files: ["photo.png": png, "ticket.pdf": Data("%PDF-1.4".utf8)])
        let id = importer.importPending()[0]
        let md = store.core.markdown(id: id)!
        XCTAssertTrue(md.hasPrefix("# Trip"), md)
        XCTAssertTrue(md.contains("![](attachments/"), md)
        XCTAssertTrue(md.contains("[ticket.pdf](attachments/"), md)
    }

    func testDropsAreAddedInTheOrderShared() throws {
        let id = note(["Log"])
        for (i, word) in ["one", "two", "three"].enumerated() {
            var d = SharedDrop(noteId: id, text: word)
            d.created = Date(timeIntervalSince1970: 1_000_000 + Double(i))
            try post(d)
        }
        importer.importPending()
        XCTAssertEqual(store.core.openEditor(id: id)!.plainText(), "Log\none\ntwo\nthree")
    }

    func testTheCatalogListsOpenNotesAndFolders() {
        let open = note(["Open note"])
        let locked = note(["Secret"])
        XCTAssertNoThrow(try store.core.setPassword(password: "pw", hint: ""))
        _ = store.core.lockNote(id: locked)
        let c = importer.catalog()
        XCTAssertTrue(c.notes.contains { $0.id == open && $0.title == "Open note" })
        XCTAssertFalse(c.notes.contains { $0.id == locked }, "locked notes can't be added to")
        XCTAssertTrue(c.folders.contains { $0.id == c.defaultFolder })

        importer.writeCatalog()
        XCTAssertEqual(inbox.readCatalog(), c)
    }
}
