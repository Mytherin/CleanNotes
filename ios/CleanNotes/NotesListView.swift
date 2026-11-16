import SwiftUI

/// A folder's notes (or All Notes, a tag, Recently Deleted), as in Apple
/// Notes: sections by date with pinned notes on top, swipe right to pin,
/// left to delete or move, touch and hold for a preview and menu, a gallery
/// view, selecting several notes, and the note count and compose button at
/// the bottom.
struct NotesListView: View {
    @EnvironmentObject private var store: NotesStore
    let place: Place
    @Binding var path: [Route]

    @State private var search = ""
    @State private var editMode: EditMode = .inactive
    @State private var selection: Set<String> = []
    @State private var moving: [String]?
    @State private var confirmForever: [String]?
    @State private var lockRequest: String?
    @State private var visible = false
    @State private var offerUndo = false
    /// Sections folded away by tapping their header.
    @State private var collapsed: Set<String> = []

    private var deletedView: Bool { place.isDeleted }

    var body: some View {
        let _ = store.revision
        let settings = store.core.settings()
        let sections = store.core.sections(place: place, search: search)
        let count = sections.reduce(0) { $0 + $1.notes.count }
        let title = store.core.title(place: place, search: "")
        Group {
            if count == 0 && search.isEmpty {
                ContentUnavailableView("No Notes", systemImage: deletedView ? "trash" : "note.text", description: Text(deletedView ? "Notes you delete stay here for 30 days." : "Tap the compose button to make a note."))
            } else if settings.gallery && !editMode.isEditing {
                GalleryView(sections: sections, place: place, path: $path, menu: noteMenu)
            } else {
                list(sections)
            }
        }
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.large)
        .searchable(text: $search, prompt: "Search")
        .onChange(of: search) { _, s in store.search = s }
        .refreshable { await store.refresh() }
        .environment(\.editMode, $editMode)
        .toolbar { toolbar(settings: settings, count: count, sections: sections) }
        .sheet(isPresented: Binding(get: { moving != nil }, set: { if !$0 { moving = nil } })) {
            MoveSheet(ids: moving ?? []) {
                moving = nil
                selection = []
                editMode = .inactive
            }
        }
        .confirmationDialog(
            (confirmForever?.count ?? 0) == 1 ? "Delete this note?" : "Delete \(confirmForever?.count ?? 0) notes?",
            isPresented: Binding(get: { confirmForever != nil }, set: { if !$0 { confirmForever = nil } }),
            titleVisibility: .visible
        ) {
            Button("Delete", role: .destructive) {
                if let ids = confirmForever {
                    store.perform { $0.deleteForever(ids: ids) }
                }
                confirmForever = nil
                selection = []
                editMode = .inactive
            }
        } message: {
            Text("This action can't be undone.")
        }
        .lockPrompts(noteId: $lockRequest)
        .alert("Undo Delete?", isPresented: $offerUndo) {
            Button("Cancel", role: .cancel) {}
            Button("Undo") { undoDelete() }
        }
        .onAppear {
            visible = true
            store.search = search
            if store.core.placeExists(place: place) { store.core.rememberPlace(place: place) }
        }
        .onDisappear { visible = false }
        .onReceive(NotificationCenter.default.publisher(for: .deviceDidShake)) { _ in
            if visible && store.core.canUndoDelete() { offerUndo = true }
        }
    }

    private func list(_ sections: [NoteSection]) -> some View {
        List(selection: $selection) {
            ForEach(sections, id: \.title) { section in
                Section {
                    if section.title.isEmpty || !collapsed.contains(section.title) {
                        ForEach(section.notes) { note in
                            NavigationLink(value: Route.note(id: note.id, isNew: false)) {
                                NoteRow(note: note)
                            }
                            .tag(note.id)
                            .swipeActions(edge: .leading, allowsFullSwipe: true) { leadingSwipe(note) }
                            .swipeActions(edge: .trailing, allowsFullSwipe: true) { trailingSwipe(note) }
                            .contextMenu {
                                noteMenu(note)
                            } preview: {
                                NotePreview(id: note.id).environmentObject(store)
                            }
                        }
                    }
                } header: {
                    if !section.title.isEmpty {
                        SectionHeader(title: section.title, collapsed: collapsed.contains(section.title)) {
                            withAnimation {
                                if collapsed.contains(section.title) {
                                    collapsed.remove(section.title)
                                } else {
                                    collapsed.insert(section.title)
                                }
                            }
                        }
                    }
                } footer: {
                    if deletedView && section.title == sections.last?.title {
                        Text("Notes are available here for 30 days. After that time, notes will be permanently deleted.")
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
    }

    @ViewBuilder
    private func leadingSwipe(_ note: NoteItem) -> some View {
        if deletedView {
            Button {
                store.perform { $0.recoverNote(id: note.id) }
            } label: { Label("Recover", systemImage: "arrow.uturn.backward") }
                .tint(.blue)
        } else {
            Button {
                store.perform { $0.togglePin(id: note.id) }
            } label: {
                Label(note.pinned ? "Unpin" : "Pin", systemImage: note.pinned ? "pin.slash.fill" : "pin.fill")
            }
            .tint(.orange)
        }
    }

    @ViewBuilder
    private func trailingSwipe(_ note: NoteItem) -> some View {
        Button(role: deletedView ? nil : .destructive) {
            delete([note.id])
        } label: { Label("Delete", systemImage: "trash") }
            .tint(.red)
        Button {
            moving = [note.id]
        } label: { Label("Move", systemImage: "folder") }
            .tint(.purple)
    }

    /// The touch-and-hold menu for a note (also the gallery's).
    @ViewBuilder
    func noteMenu(_ note: NoteItem) -> some View {
        if deletedView {
            Button { store.perform { $0.recoverNote(id: note.id) } } label: { Label("Recover", systemImage: "arrow.uturn.backward") }
            Button { moving = [note.id] } label: { Label("Move", systemImage: "folder") }
            Button(role: .destructive) { delete([note.id]) } label: { Label("Delete", systemImage: "trash") }
        } else {
            Button {
                store.perform { $0.togglePin(id: note.id) }
            } label: { Label(note.pinned ? "Unpin Note" : "Pin Note", systemImage: note.pinned ? "pin.slash" : "pin") }
            if note.locked {
                Button {
                    store.perform { $0.removeLock(id: note.id) }
                } label: { Label("Remove Lock", systemImage: "lock.open") }
                    .disabled(store.core.isClosed(id: note.id))
            } else {
                Button { lockRequest = note.id } label: { Label("Lock Note", systemImage: "lock") }
            }
            if !note.locked, let shared = SharedNote(note: note, store: store) {
                ShareLink(item: shared, subject: Text(note.title), preview: SharePreview(note.title)) {
                    Label("Share Note", systemImage: "square.and.arrow.up")
                }
            }
            Button { moving = [note.id] } label: { Label("Move", systemImage: "folder") }
            Button {
                store.perform { _ = $0.duplicateNote(id: note.id) }
            } label: { Label("Duplicate", systemImage: "plus.square.on.square") }
            Divider()
            Button(role: .destructive) { delete([note.id]) } label: { Label("Delete", systemImage: "trash") }
        }
    }

    @ToolbarContentBuilder
    private func toolbar(settings: AppSettings, count: Int, sections: [NoteSection]) -> some ToolbarContent {
        ToolbarItem(placement: .topBarTrailing) {
            if editMode.isEditing {
                Button("Done") {
                    editMode = .inactive
                    selection = []
                }
                .bold()
            } else {
                viewMenu(settings)
            }
        }
        ToolbarItemGroup(placement: .bottomBar) {
            if editMode.isEditing {
                let ids = selection.isEmpty ? sections.flatMap { $0.notes.map(\.id) } : Array(selection)
                Button(selection.isEmpty ? "Move All" : "Move") { moving = ids }
                    .disabled(ids.isEmpty)
                Spacer()
                if deletedView {
                    Button(selection.isEmpty ? "Recover All" : "Recover") {
                        store.perform { core in ids.forEach { core.recoverNote(id: $0) } }
                        selection = []
                        editMode = .inactive
                    }
                    .disabled(ids.isEmpty)
                    Spacer()
                }
                Button(selection.isEmpty ? "Delete All" : "Delete") { delete(ids) }
                    .disabled(ids.isEmpty)
            } else {
                Spacer()
                Text(notesCount(count))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Button {
                    newNote()
                } label: { Image(systemName: "square.and.pencil") }
                    .disabled(deletedView)
                    .accessibilityLabel("New Note")
            }
        }
    }

    private func viewMenu(_ settings: AppSettings) -> some View {
        Menu {
            if !deletedView {
                Picker("View", selection: Binding(
                    get: { settings.gallery },
                    set: { g in update(settings) { $0.gallery = g } }
                )) {
                    Label("View as List", systemImage: "list.bullet").tag(false)
                    Label("View as Gallery", systemImage: "square.grid.2x2").tag(true)
                }
            }
            Button {
                editMode = .active
            } label: { Label("Select Notes", systemImage: "checkmark.circle") }
            Menu {
                Picker("Sort By", selection: Binding(
                    get: { settings.sortBy },
                    set: { s in update(settings) { $0.sortBy = s } }
                )) {
                    Text("Date Edited").tag(SortOrder.dateEdited)
                    Text("Date Created").tag(SortOrder.dateCreated)
                    Text("Title").tag(SortOrder.title)
                }
                Toggle("Group By Date", isOn: Binding(
                    get: { settings.groupByDate },
                    set: { g in update(settings) { $0.groupByDate = g } }
                ))
            } label: {
                Label("Sort By", systemImage: "arrow.up.arrow.down")
            }
        } label: {
            Image(systemName: "ellipsis.circle")
        }
    }

    private func update(_ settings: AppSettings, _ change: (inout AppSettings) -> Void) {
        var s = settings
        change(&s)
        store.perform { $0.setSettings(new: s) }
    }

    private func newNote() {
        store.saveEditor()
        let n = store.core.newNote(place: place)
        if n.place != place {
            path.append(.list(n.place))
        }
        path.append(.note(id: n.id, isNew: true))
    }

    /// Deletes notes (to Recently Deleted, or for good from there, after
    /// asking).
    private func delete(_ ids: [String]) {
        guard !ids.isEmpty else { return }
        store.saveEditor()
        if store.core.deletesForever(ids: ids) {
            confirmForever = ids
            return
        }
        store.perform { $0.trashNotes(ids: ids, place: place) }
        selection = []
        editMode = .inactive
    }

    /// Shake to undo: the notes come back pinned and dated as before.
    private func undoDelete() {
        guard let restored = store.core.undoDelete() else { return }
        store.changed()
        if restored.place != place && store.core.placeExists(place: restored.place) {
            path.append(.list(restored.place))
        }
    }
}

/// A date section's header ("Pinned", "Today", …): tap it to fold the
/// section away, as in Apple Notes.
private struct SectionHeader: View {
    let title: String
    let collapsed: Bool
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            HStack {
                Text(title)
                    .font(.title3.bold())
                    .foregroundStyle(.primary)
                Spacer()
                Image(systemName: "chevron.down")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Color.notesAccent)
                    .rotationEffect(.degrees(collapsed ? -90 : 0))
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .textCase(nil)
        .accessibilityLabel(title)
        .accessibilityHint(collapsed ? "Shows the notes in this section." : "Hides the notes in this section.")
    }
}

/// A note in the list: its title, then its time and first line, and its
/// folder when the list mixes folders.
struct NoteRow: View {
    let note: NoteItem

    /// Text with a search's matches in bold (as Apple Notes shows search
    /// results); just the text when not searching.
    static func text(_ plain: String, parts: [TextPart]) -> Text {
        guard !parts.isEmpty else { return Text(plain) }
        return parts.reduce(Text("")) { shown, part in
            shown + (part.matched ? Text(part.text).bold().foregroundStyle(.primary) : Text(part.text))
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 4) {
                NoteRow.text(note.title, parts: note.titleParts)
                    .font(.headline)
                    .lineLimit(1)
                if note.locked {
                    Image(systemName: "lock.fill")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            HStack(spacing: 6) {
                Text(note.time)
                    .foregroundStyle(.primary)
                NoteRow.text(note.snippet, parts: note.snippetParts)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            .font(.subheadline)
            if let folder = note.folder {
                Label(folder, systemImage: "folder")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 2)
    }
}

/// The note's text, for the touch-and-hold preview.
struct NotePreview: View {
    @EnvironmentObject private var store: NotesStore
    let id: String

    var body: some View {
        let closed = store.core.isClosed(id: id)
        let text = closed ? nil : store.core.openEditor(id: id).map { NoteTextRenderer.attributed($0.state().paragraphs, textSize: store.core.settings().textSize) }
        Group {
            if let text {
                AttributedTextPreview(text: text)
            } else {
                VStack(spacing: 12) {
                    Image(systemName: "lock.fill").font(.largeTitle)
                    Text("This note is locked.")
                }
                .foregroundStyle(.secondary)
            }
        }
        .frame(width: 320, height: 420, alignment: .topLeading)
        .background(Color(.systemBackground))
    }
}

/// Gallery view: notes as cards with a preview of their text.
struct GalleryView<ItemMenu: View>: View {
    @EnvironmentObject private var store: NotesStore
    let sections: [NoteSection]
    let place: Place
    @Binding var path: [Route]
    let menu: (NoteItem) -> ItemMenu

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 20) {
                ForEach(sections, id: \.title) { section in
                    if !section.title.isEmpty {
                        Text(section.title).font(.title3.bold()).padding(.horizontal)
                    }
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), spacing: 16)], spacing: 20) {
                        ForEach(section.notes) { note in
                            Button {
                                path.append(.note(id: note.id, isNew: false))
                            } label: {
                                GalleryCard(note: note)
                            }
                            .buttonStyle(.plain)
                            .contextMenu { menu(note) } preview: { NotePreview(id: note.id).environmentObject(store) }
                        }
                    }
                    .padding(.horizontal)
                }
            }
            .padding(.vertical)
        }
    }
}

private struct GalleryCard: View {
    @EnvironmentObject private var store: NotesStore
    let note: NoteItem

    var body: some View {
        VStack(spacing: 6) {
            ZStack(alignment: .topLeading) {
                RoundedRectangle(cornerRadius: 12)
                    .fill(Color(.secondarySystemGroupedBackground))
                    .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Color(.separator), lineWidth: 0.5))
                if note.locked {
                    Image(systemName: "lock.fill")
                        .font(.title)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    Text(note.empty ? note.title : "\(note.title)\n\(note.snippet)")
                        .font(.system(size: 9))
                        .foregroundStyle(.primary)
                        .padding(10)
                }
            }
            .frame(height: 170)
            Text(note.title).font(.subheadline.bold()).lineLimit(1)
            Text(note.time).font(.caption).foregroundStyle(.secondary)
        }
    }
}

extension Notification.Name {
    static let deviceDidShake = Notification.Name("CleanNotesDeviceDidShake")
}

extension UIWindow {
    open override func motionEnded(_ motion: UIEvent.EventSubtype, with event: UIEvent?) {
        if motion == .motionShake {
            NotificationCenter.default.post(name: .deviceDidShake, object: nil)
        }
        super.motionEnded(motion, with: event)
    }
}
