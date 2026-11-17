import UIKit
import XCTest
@testable import CleanNotes

/// The note editor as the app wires it: a real text view, the coordinator
/// and the Rust editing engine, in a notes library of its own. Typing goes
/// key by key through the text view's delegate, as the keyboard's does, so
/// the delegate decides what the text view and the engine do. After every
/// edit the two must agree.
final class NoteEditorTests: XCTestCase {
    private var dir: URL!
    private var store: NotesStore!

    override func setUpWithError() throws {
        dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        store = NotesStore(directory: dir)
        XCTAssertNil(store.startupError)
    }

    override func tearDownWithError() throws {
        store = nil
        try? FileManager.default.removeItem(at: dir)
    }

    // ---- Helpers ----

    /// A new note in a text view (it starts with a Title paragraph).
    private func newNote() -> (NoteTextView, NoteEditorView.Coordinator) {
        let n = store.core.newNote(place: .folder(id: store.core.defaultFolder()))
        let editor = store.core.newNoteEditor(id: n.id)
        let c = NoteEditorView.Coordinator(editor: editor, store: store, bridge: EditorBridge())
        let tv = NoteTextView()
        tv.frame = CGRect(x: 0, y: 0, width: 390, height: 800)
        c.attach(tv)
        return (tv, c)
    }

    /// Types `text` a character at a time ("\n" is Return).
    private func type(_ text: String, into tv: NoteTextView) {
        for ch in text {
            key(String(ch), replacing: tv.selectedRange, in: tv)
        }
    }

    /// Backspace: the character (or selection) before the caret.
    private func backspace(_ tv: NoteTextView) {
        var r = tv.selectedRange
        if r.length == 0 {
            if r.location == 0 { return }
            r = (tv.text as NSString).rangeOfComposedCharacterSequence(at: r.location - 1)
        }
        key("", replacing: r, in: tv)
    }

    /// One key, as the keyboard sends it: the delegate is asked first, and
    /// only if it allows the change does the text view make it (and then say
    /// it changed). `insertText`/`deleteBackward` called from a test don't
    /// reliably ask the delegate (the text view isn't being edited), so the
    /// engine would only see the text afterwards, without list handling.
    private func key(_ s: String, replacing r: NSRange, in tv: NoteTextView) {
        guard let d = tv.delegate else { return XCTFail("no delegate") }
        guard d.textView?(tv, shouldChangeTextIn: r, replacementText: s) ?? true else { return }
        tv.textStorage.replaceCharacters(in: r, with: NSAttributedString(string: s, attributes: tv.typingAttributes))
        tv.selectedRange = NSRange(location: r.location + (s as NSString).length, length: 0)
        d.textViewDidChange?(tv)
    }

    private func paragraphs(_ c: NoteEditorView.Coordinator) -> [Paragraph] {
        c.editor.state().paragraphs
    }

    private func text(_ p: Paragraph) -> String {
        p.runs.map(\.text).joined()
    }

    /// The text view shows the engine's text and selection.
    private func assertInStep(_ tv: NoteTextView, _ c: NoteEditorView.Coordinator, file: StaticString = #filePath, line: UInt = #line) {
        let state = c.editor.state()
        XCTAssertEqual(tv.text, c.editor.plainText(), "text", file: file, line: line)
        XCTAssertEqual(tv.selectedRange.location, Int(state.selectionStart), "caret", file: file, line: line)
    }

    // ---- Typing ----

    func testReturnAfterTheTitleStartsBodyText() {
        let (tv, c) = newNote()
        type("Groceries\nmilk", into: tv)
        let ps = paragraphs(c)
        XCTAssertEqual(ps.count, 2)
        XCTAssertEqual(ps[0].style, .title)
        XCTAssertEqual(text(ps[0]), "Groceries")
        XCTAssertEqual(ps[1].style, .body)
        XCTAssertEqual(text(ps[1]), "milk")
        assertInStep(tv, c)
    }

    func testDashSpaceStartsAListThatReturnContinuesAndEnds() {
        let (tv, c) = newNote()
        type("List\n- eggs\nmilk", into: tv)
        var ps = paragraphs(c)
        XCTAssertEqual(ps.map(\.style), [.title, .dashed, .dashed])
        XCTAssertEqual(ps.map { text($0) }, ["List", "eggs", "milk"])
        XCTAssertEqual(tv.text, "List\neggs\nmilk", "the marker is drawn, not typed")
        assertInStep(tv, c)

        // Return on an empty item ends the list.
        type("\n\n", into: tv)
        ps = paragraphs(c)
        XCTAssertEqual(ps.last?.style, .body)
        XCTAssertEqual(ps.count, 4)
        assertInStep(tv, c)
    }

    func testBackspaceAtAListItemsStartMakesItText() {
        let (tv, c) = newNote()
        type("T\n- a", into: tv)
        tv.selectedRange = NSRange(location: 2, length: 0)
        backspace(tv)
        let ps = paragraphs(c)
        XCTAssertEqual(ps.count, 2)
        XCTAssertEqual(ps[1].style, .body)
        XCTAssertEqual(tv.text, "T\na")
        assertInStep(tv, c)
    }

    func testEmojiCountInUTF16AsUIKitDoes() {
        let (tv, c) = newNote()
        type("a👍🏽b", into: tv)
        XCTAssertEqual(c.editor.plainText(), "a👍🏽b")
        XCTAssertEqual(c.editor.state().selectionStart, 6, "👍🏽 is four UTF-16 units")
        assertInStep(tv, c)
        backspace(tv)
        backspace(tv)
        XCTAssertEqual(c.editor.plainText(), "a")
        assertInStep(tv, c)
    }

    func testUndoGoesToTheEngine() {
        let (tv, c) = newNote()
        type("Hello", into: tv)
        guard let undo = tv.undoManager else { return XCTFail("no undo manager") }
        XCTAssertTrue(undo.canUndo)
        // Typing is grouped into steps by time: undo them all.
        while undo.canUndo { undo.undo() }
        XCTAssertEqual(c.editor.plainText(), "")
        assertInStep(tv, c)
        while undo.canRedo { undo.redo() }
        XCTAssertEqual(c.editor.plainText(), "Hello")
        assertInStep(tv, c)
    }

    // ---- Commands and what's drawn ----

    func testChecklistCommandsAndTheMarksTheyDraw() {
        let (tv, c) = newNote()
        type("Trip\npassport\ntickets", into: tv)
        tv.selectedRange = NSRange(location: 5, length: 16)
        c.command(.toggleChecklist)
        XCTAssertEqual(paragraphs(c).map(\.style), [.title, .checklist, .checklist])
        XCTAssertTrue(c.formatting?.isList ?? false)
        XCTAssertEqual(c.formatting?.checklistHasUnchecked, true)

        c.command(.checkAll)
        XCTAssertTrue(paragraphs(c)[1...].allSatisfy(\.checked))
        let mark = tv.storage.attribute(.cnParagraph, at: 6, effectiveRange: nil) as? ParagraphMark
        XCTAssertEqual(mark?.style, .checklist)
        XCTAssertEqual(mark?.marker, "☑")

        c.command(.deleteChecked)
        XCTAssertEqual(paragraphs(c).map { text($0) }, ["Trip"], "every item was ticked")
        assertInStep(tv, c)
    }

    func testListMarkersComeFromTheCore() {
        let (tv, c) = newNote()
        type("N\n1. one\ntwo\n\n* dot", into: tv)
        let ps = paragraphs(c)
        // Return on the empty third item made it text; "* " made that a bullet.
        XCTAssertEqual(ps.map(\.marker), ["", "1.", "2.", "•"])
        let printed = NoteTextRenderer.printable(ps, textSize: 15).string
        XCTAssertTrue(printed.contains("2.\ttwo"))
        XCTAssertTrue(printed.contains("•\tdot"))
        assertInStep(tv, c)
    }

    // ---- Tables ----

    func testTablesAreInsertedTypedInAndDrawnAsCells() {
        let (tv, c) = newNote()
        type("Plan\n", into: tv)
        c.command(.insertTable)
        XCTAssertEqual(c.formatting?.inTable, true)
        type("Day", into: tv)
        c.command(.nextCell)
        type("What\nWhere", into: tv)
        let cells = paragraphs(c).compactMap(\.cell)
        XCTAssertEqual(cells.count, 9, "Return in a cell is a line break in it")
        assertInStep(tv, c)

        c.command(.toggleHeaderRow)
        XCTAssertEqual(c.formatting?.headerRow, true)
        // The first cell's text carries its place in the table, for drawing.
        let start = (tv.text as NSString).range(of: "Day").location
        let mark = tv.storage.attribute(.cnParagraph, at: start, effectiveRange: nil) as? ParagraphMark
        XCTAssertEqual(mark?.cell?.row, 0)
        XCTAssertEqual(mark?.cell?.columns, 3)
        XCTAssertEqual(mark?.cell?.header, true)
        let font = tv.storage.attribute(.font, at: start, effectiveRange: nil) as? UIFont
        XCTAssertTrue(font?.fontDescriptor.symbolicTraits.contains(.traitBold) ?? false, "header cells are bold")

        let items = TableActions.items(c.formatting) { _ in }
        XCTAssertEqual(items.count, 4, "cells, rows and columns, headers, deleting")
        c.command(.deleteTable)
        XCTAssertTrue(paragraphs(c).allSatisfy { $0.cell == nil })
        assertInStep(tv, c)
    }

    // ---- Sharing ----

    /// A solid picture of the given size in pixels.
    private func picture(width: Int, height: Int) -> UIImage {
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        return UIGraphicsImageRenderer(size: CGSize(width: width, height: height), format: format).image { ctx in
            UIColor.gray.setFill()
            ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))
        }
    }

    func testScannedPagesBecomeOnePDFPagePerImageInItsShape() throws {
        XCTAssertNil(ScannedDocument.pdf([]))
        let pdf = try XCTUnwrap(ScannedDocument.pdf([picture(width: 850, height: 1100), picture(width: 1200, height: 600)]))
        let doc = try XCTUnwrap(CGPDFDocument(CGDataProvider(data: pdf as CFData)!))
        XCTAssertEqual(doc.numberOfPages, 2)
        let first = try XCTUnwrap(doc.page(at: 1)).getBoxRect(.mediaBox)
        let second = try XCTUnwrap(doc.page(at: 2)).getBoxRect(.mediaBox)
        XCTAssertEqual(first.width, ScannedDocument.pageWidth, accuracy: 0.5)
        XCTAssertEqual(first.height, ScannedDocument.pageWidth * 1100 / 850, accuracy: 0.5)
        XCTAssertEqual(second.height, ScannedDocument.pageWidth / 2, accuracy: 0.5)
    }

    func testAScanIsAttachedAsAPDFAtTheCaret() throws {
        let (tv, c) = newNote()
        type("Receipt\n", into: tv)
        let pdf = try XCTUnwrap(ScannedDocument.pdf([picture(width: 400, height: 500)]))
        c.attachScan(pdf)
        let md = c.editor.markdown()
        XCTAssertTrue(md.contains("[Scanned Document.pdf]("), md)
        // The link opens the very PDF.
        let link = try XCTUnwrap(md.range(of: #"\[Scanned Document\.pdf\]\(([^)]+)\)"#, options: .regularExpression))
        let address = md[link].dropFirst("[Scanned Document.pdf](".count).dropLast()
        let reference = String(address.dropFirst(address.hasPrefix("attachments/") ? "attachments/".count : 0))
        let path = try store.core.openAttachment(reference: reference, label: ScannedDocument.fileName)
        XCTAssertEqual(try Data(contentsOf: URL(fileURLWithPath: path)), pdf)
        assertInStep(tv, c)
    }

    func testSharingGivesTextAndRichText() throws {
        let (tv, c) = newNote()
        type("List\n[] eggs", into: tv)
        let shared = SharedNote(title: "List", textSize: 15, editor: c.editor)
        XCTAssertEqual(shared.text(), "List\n☐ eggs")
        let rtf = try shared.rtf()
        XCTAssertTrue(String(decoding: rtf.prefix(5), as: UTF8.self).hasPrefix("{\\rtf"))
        assertInStep(tv, c)
    }
}
