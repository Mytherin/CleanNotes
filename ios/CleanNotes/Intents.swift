import AppIntents
import Foundation

// Shortcuts and Siri, as Apple Notes offers them: make a note (opened, or
// in the background with text given), add to a note, open a note or a
// folder, and find notes. They work on the app's own notes
// (`NotesStore.shared`), started in the background when needed.
//
// Locked notes can be found and opened by title (the app then asks for the
// password), but nothing outside the app reads or changes their text.

// ---- Notes and folders ----

/// A note, as Shortcuts and Siri name it.
struct NoteEntity: AppEntity {
    static var typeDisplayRepresentation = TypeDisplayRepresentation(name: "Note")
    static var defaultQuery = NoteQuery()

    var id: String
    var title: String
    /// The first line after the title ("Locked" for a locked note).
    var snippet: String

    init(_ item: NoteItem) {
        id = item.id
        title = item.title
        snippet = item.snippet
    }

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(title: "\(title)", subtitle: "\(snippet)")
    }
}

/// Finds notes as the app's search does: every word must match. Recently
/// Deleted is left out.
struct NoteQuery: EntityStringQuery {
    func entities(for identifiers: [NoteEntity.ID]) async throws -> [NoteEntity] {
        await MainActor.run {
            let core = NotesStore.shared.core
            return identifiers
                .compactMap { core.note(id: $0, place: .allNotes) }
                .filter { !$0.deleted }
                .map(NoteEntity.init)
        }
    }

    func entities(matching string: String) async throws -> [NoteEntity] {
        await Self.notes(search: string)
    }

    /// The notes most recently edited (pinned ones first), as All Notes
    /// lists them.
    func suggestedEntities() async throws -> [NoteEntity] {
        Array(await Self.notes(search: "").prefix(50))
    }

    @MainActor
    static func notes(search: String) -> [NoteEntity] {
        let store = NotesStore.shared
        store.saveEditor()
        return store.core.sections(place: .allNotes, search: search)
            .flatMap(\.notes)
            .map(NoteEntity.init)
    }
}

/// A folder, as Shortcuts and Siri name it.
struct FolderEntity: AppEntity {
    static var typeDisplayRepresentation = TypeDisplayRepresentation(name: "Folder")
    static var defaultQuery = FolderQuery()

    var id: String
    var name: String
    /// Its account ("On My iPhone", an S3 account's name).
    var account: String

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(title: "\(name)", subtitle: "\(account)")
    }
}

/// Every folder of the accounts that are on, as the Folders screen lists
/// them.
struct FolderQuery: EntityQuery {
    func entities(for identifiers: [FolderEntity.ID]) async throws -> [FolderEntity] {
        let all = await Self.folders()
        return identifiers.compactMap { id in all.first { $0.id == id } }
    }

    func suggestedEntities() async throws -> [FolderEntity] {
        await Self.folders()
    }

    @MainActor
    static func folders() -> [FolderEntity] {
        NotesStore.shared.core.overview().accounts.flatMap { account in
            account.folders.map { FolderEntity(id: $0.id, name: $0.name, account: account.name) }
        }
    }
}

/// What Shortcuts shows when an action can't be done.
struct NotesIntentError: Error, CustomLocalizedStringResourceConvertible {
    let text: String
    var localizedStringResource: LocalizedStringResource { LocalizedStringResource(stringLiteral: text) }
}

// ---- Actions ----

/// A new, empty note, open with the keyboard up (as the quick action).
struct NewNoteIntent: AppIntent {
    static var title: LocalizedStringResource = "New Note"
    static var description = IntentDescription("Opens a new note, ready to type in.")
    static var openAppWhenRun = true

    @Parameter(title: "Folder", description: "Where the note goes. Without one, it goes where new notes go.")
    var folder: FolderEntity?

    static var parameterSummary: some ParameterSummary {
        Summary("New note in \(\.$folder)")
    }

    @MainActor
    func perform() async throws -> some IntentResult {
        AppRequests.shared.pending = .newNote(folder: folder?.id)
        return .result()
    }
}

/// A note with the text given, made without opening the app.
struct CreateNoteIntent: AppIntent {
    static var title: LocalizedStringResource = "Create Note"
    static var description = IntentDescription("Makes a note with the text given. Its first line is the note's title.")

    @Parameter(title: "Text", inputOptions: String.IntentInputOptions(multiline: true), requestValueDialog: "What should the note say?")
    var text: String

    @Parameter(title: "Folder", description: "Where the note goes. Without one, it goes where new notes go.")
    var folder: FolderEntity?

    static var parameterSummary: some ParameterSummary {
        Summary("Create a note with \(\.$text)") {
            \.$folder
        }
    }

    @MainActor
    func perform() async throws -> some IntentResult & ReturnsValue<NoteEntity> {
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw NotesIntentError(text: "There's no text for the note.")
        }
        let store = NotesStore.shared
        store.saveEditor()
        let n = store.core.createNote(folder: folder?.id, text: text)
        store.changedOutside()
        guard let item = store.core.note(id: n.id, place: n.place) else {
            throw NotesIntentError(text: "The note couldn't be made.")
        }
        return .result(value: NoteEntity(item))
    }
}

/// Adds text to the end of a note, on a line of its own.
struct AppendToNoteIntent: AppIntent {
    static var title: LocalizedStringResource = "Append to Note"
    static var description = IntentDescription("Adds text to the end of a note. Locked notes can't be changed this way.")

    @Parameter(title: "Note")
    var note: NoteEntity

    @Parameter(title: "Text", inputOptions: String.IntentInputOptions(multiline: true), requestValueDialog: "What should be added?")
    var text: String

    static var parameterSummary: some ParameterSummary {
        Summary("Append \(\.$text) to \(\.$note)")
    }

    @MainActor
    func perform() async throws -> some IntentResult & ReturnsValue<NoteEntity> {
        let store = NotesStore.shared
        // The open note's typing first, so it isn't lost.
        store.saveEditor()
        do {
            try store.core.addText(id: note.id, text: text)
        } catch {
            throw NotesIntentError(text: message(error))
        }
        store.changedOutside()
        let item = store.core.note(id: note.id, place: .allNotes)
        return .result(value: item.map(NoteEntity.init) ?? note)
    }
}

/// Opens a note in the app.
struct OpenNoteIntent: OpenIntent {
    static var title: LocalizedStringResource = "Open Note"
    static var description = IntentDescription("Opens a note in the app.")
    static var openAppWhenRun = true

    @Parameter(title: "Note")
    var target: NoteEntity

    @MainActor
    func perform() async throws -> some IntentResult {
        AppRequests.shared.pending = .openNote(id: target.id, editing: false)
        return .result()
    }
}

/// Opens a folder's notes list in the app.
struct OpenFolderIntent: OpenIntent {
    static var title: LocalizedStringResource = "Open Folder"
    static var description = IntentDescription("Shows a folder's notes in the app.")
    static var openAppWhenRun = true

    @Parameter(title: "Folder")
    var target: FolderEntity

    @MainActor
    func perform() async throws -> some IntentResult {
        AppRequests.shared.pending = .openPlace(.folder(id: target.id))
        return .result()
    }
}

/// The notes with every word searched for, as the app's search finds them.
struct FindNotesIntent: AppIntent {
    static var title: LocalizedStringResource = "Find Notes"
    static var description = IntentDescription("Finds the notes that have every word searched for, in their title or text.")

    @Parameter(title: "Search", requestValueDialog: "What should be searched for?")
    var search: String

    static var parameterSummary: some ParameterSummary {
        Summary("Find notes with \(\.$search)")
    }

    @MainActor
    func perform() async throws -> some IntentResult & ReturnsValue<[NoteEntity]> {
        .result(value: NoteQuery.notes(search: search))
    }
}

// ---- Siri ----

/// What Siri understands without setting anything up, and what the
/// Shortcuts app and Spotlight offer for the app.
struct NotesShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: NewNoteIntent(),
            phrases: [
                "New note in \(.applicationName)",
                "Make a note in \(.applicationName)",
                "Start a \(.applicationName) note",
            ],
            shortTitle: "New Note",
            systemImageName: "square.and.pencil"
        )
        AppShortcut(
            intent: OpenNoteIntent(),
            phrases: [
                "Open \(\.$target) in \(.applicationName)",
                "Show \(\.$target) in \(.applicationName)",
            ],
            shortTitle: "Open Note",
            systemImageName: "note.text"
        )
        AppShortcut(
            intent: AppendToNoteIntent(),
            phrases: [
                "Add to \(\.$note) in \(.applicationName)",
                "Append to \(\.$note) in \(.applicationName)",
            ],
            shortTitle: "Append to Note",
            systemImageName: "text.append"
        )
        AppShortcut(
            intent: OpenFolderIntent(),
            phrases: [
                "Open the \(\.$target) folder in \(.applicationName)",
            ],
            shortTitle: "Open Folder",
            systemImageName: "folder"
        )
        AppShortcut(
            intent: FindNotesIntent(),
            phrases: [
                "Search \(.applicationName)",
                "Find notes in \(.applicationName)",
            ],
            shortTitle: "Find Notes",
            systemImageName: "magnifyingglass"
        )
    }
}
