import SwiftUI
import UniformTypeIdentifiers

/// A note for the share sheet, as Apple Notes' "Send a Copy": plain text
/// (the core's: list markers and checkboxes written out) for Messages and
/// most apps, and rich text for those that take it (Mail, Pages, Notes).
/// Both are made only when the share sheet asks for them.
struct SharedNote: Transferable {
    let title: String
    let textSize: Float
    /// The note as plain text, from the core.
    let text: @Sendable () -> String
    /// The note's paragraphs, for the rich text.
    let paragraphs: @Sendable () -> [Paragraph]

    static var transferRepresentation: some TransferRepresentation {
        // Plain text first: apps that take text (Messages) get text, not an
        // .rtf attachment.
        ProxyRepresentation(exporting: { (note: SharedNote) in note.text() })
        DataRepresentation(exportedContentType: .rtf) { note in try note.rtf() }
    }

    func rtf() throws -> Data {
        let rich = NSMutableAttributedString(attributedString: NoteTextRenderer.printable(paragraphs(), textSize: textSize))
        let all = NSRange(location: 0, length: rich.length)
        // The app's own attributes mean nothing elsewhere.
        rich.removeAttribute(.cnParagraph, range: all)
        rich.removeAttribute(.cnTag, range: all)
        return try rich.data(from: all, documentAttributes: [.documentType: NSAttributedString.DocumentType.rtf])
    }
}

extension SharedNote {
    /// The open note, with the changes not saved yet.
    init(title: String, textSize: Float, editor: NoteEditor) {
        self.init(
            title: title,
            textSize: textSize,
            text: { editor.sharedText() },
            paragraphs: { editor.state().paragraphs }
        )
    }

    /// A note in a list (nil if it's locked and closed).
    init?(note: NoteItem, store: NotesStore) {
        let core = store.core
        let id = note.id
        guard !core.isClosed(id: id) else { return nil }
        self.init(
            title: note.title,
            textSize: core.settings().textSize,
            text: { core.sharedText(id: id) ?? "" },
            paragraphs: { core.openEditor(id: id)?.state().paragraphs ?? [] }
        )
    }
}
