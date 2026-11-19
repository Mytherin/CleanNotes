import SwiftUI
import ImageIO

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
        withToolbar(settings: settings, count: count, sections: sections) {
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
        // Selection only while selecting notes: otherwise a tap would also
        // select the note it opens, and it would stay highlighted when you
        // come back to the list (Apple Notes clears it).
        List(selection: editMode.isEditing ? $selection : nil) {
            ForEach(sections, id: \.title) { section in
                Section {
                    if section.title.isEmpty || !collapsed.contains(section.title) {
                        ForEach(section.notes) { note in
                            // A link without its chevron, as Apple Notes' rows.
                            NoteRow(note: note)
                                .background {
                                    NavigationLink(value: Route.note(id: note.id, isNew: false)) { EmptyView() }
                                        .opacity(0)
                                }
                                .listRowInsets(EdgeInsets(top: 8, leading: 26, bottom: 8, trailing: 16))
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
        // Wide cards, close to the screen's edges, as in Apple Notes.
        .contentMargins(.horizontal, 8, for: .scrollContent)
        .listSectionSpacing(18)
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

    /// The list with its toolbars. On iOS 26 as in Apple Notes: the note
    /// count under the title, and Search and the compose button floating at
    /// the bottom; before, the count between them in the bottom toolbar.
    @ViewBuilder
    private func withToolbar<Content: View>(settings: AppSettings, count: Int, sections: [NoteSection], @ViewBuilder _ content: () -> Content) -> some View {
        if #available(iOS 26.0, *) {
            Group(content: content)
                .navigationSubtitle(editMode.isEditing || (count == 0 && search.isEmpty) ? "" : notesCount(count))
                .toolbar { glassToolbar(settings: settings, sections: sections) }
        } else {
            Group(content: content)
                .toolbar { toolbar(settings: settings, count: count, sections: sections) }
        }
    }

    @available(iOS 26.0, *)
    @ToolbarContentBuilder
    private func glassToolbar(settings: AppSettings, sections: [NoteSection]) -> some ToolbarContent {
        ToolbarItem(placement: .topBarTrailing) { topButton(settings) }
        if editMode.isEditing {
            selectionBar(sections: sections)
        } else {
            DefaultToolbarItem(kind: .search, placement: .bottomBar)
            ToolbarSpacer(.fixed, placement: .bottomBar)
            ToolbarItem(placement: .bottomBar) { composeButton.tint(Color.primary) }
        }
    }

    @ViewBuilder
    private func topButton(_ settings: AppSettings) -> some View {
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

    private var composeButton: some View {
        Button {
            newNote()
        } label: { Image(systemName: "square.and.pencil") }
            .disabled(deletedView)
            .accessibilityLabel("New Note")
    }

    /// Selecting notes: Move, Recover and Delete (all of them with none
    /// selected).
    @ToolbarContentBuilder
    private func selectionBar(sections: [NoteSection]) -> some ToolbarContent {
        ToolbarItemGroup(placement: .bottomBar) {
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
        }
    }

    @ToolbarContentBuilder
    private func toolbar(settings: AppSettings, count: Int, sections: [NoteSection]) -> some ToolbarContent {
        ToolbarItem(placement: .topBarTrailing) { topButton(settings) }
        if editMode.isEditing {
            selectionBar(sections: sections)
        } else {
            ToolbarItemGroup(placement: .bottomBar) {
                Spacer()
                Text(notesCount(count))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                composeButton
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
            if #available(iOS 26.0, *) {
                Image(systemName: "ellipsis")
            } else {
                Image(systemName: "ellipsis.circle")
            }
        }
        .accessibilityLabel("More")
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
                    .font(.title2.bold())
                    .foregroundStyle(Color.primary)
                Spacer()
                // As in Apple Notes, only a folded section shows its chevron.
                if collapsed {
                    Image(systemName: "chevron.forward")
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(Color.notesAccent)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .textCase(nil)
        // Lined up with the cards' edge, not their text.
        .padding(.leading, -20)
        .padding(.bottom, 2)
        .accessibilityLabel(title)
        .accessibilityHint(collapsed ? "Shows the notes in this section." : "Hides the notes in this section.")
    }
}

/// A note in the list: its title, then its time and first line, and its
/// folder when the list mixes folders. A tab note shows its page's title,
/// its site in place of the first line, and its picture on the right.
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
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
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
                    NoteRow.text(note.snippet, parts: note.snippetParts)
                        .lineLimit(1)
                }
                .font(.subheadline)
                .foregroundStyle(.secondary)
                if let folder = note.folder {
                    Label(folder, systemImage: "folder")
                        .labelStyle(FolderLabelStyle())
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
            if let name = note.thumbnail {
                Thumbnail(name: name)
            } else if let tab = note.tab, tab.thumbnail != nil {
                // A tab note's picture.
                TabPicture(page: tab, cornerRadius: 6, compact: true)
                    .frame(width: 44, height: 44)
            }
        }
        .padding(.vertical, 4)
    }
}

/// The folder line: a small folder icon close to its name.
private struct FolderLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 3) {
            configuration.icon.imageScale(.small)
            configuration.title
        }
    }
}

/// A note's first picture beside its row, as in Apple Notes.
private struct Thumbnail: View {
    @EnvironmentObject private var store: NotesStore
    let name: String

    var body: some View {
        let size: CGFloat = 54
        Group {
            if let image = ThumbnailCache.shared.image(name: name, path: store.core.attachmentPath(name: name), side: size) {
                Image(uiImage: image).resizable().scaledToFill()
            } else {
                Color(.tertiarySystemFill)
            }
        }
        .frame(width: size, height: size)
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).strokeBorder(Color(.separator), lineWidth: 0.5))
        .accessibilityHidden(true)
    }
}

/// Small copies of the notes' pictures, made once each (pictures are
/// named by their content, so a name always means the same picture).
final class ThumbnailCache {
    static let shared = ThumbnailCache()
    private let cache = NSCache<NSString, UIImage>()

    func image(name: String, path: String?, side: CGFloat) -> UIImage? {
        if let hit = cache.object(forKey: name as NSString) { return hit }
        // Not on this device yet (still syncing): try again next time.
        guard let path else { return nil }
        let pixels = side * UIScreen.main.scale
        let options = [kCGImageSourceShouldCache: false] as CFDictionary
        guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, options),
              let small = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                  kCGImageSourceCreateThumbnailFromImageAlways: true,
                  kCGImageSourceCreateThumbnailWithTransform: true,
                  kCGImageSourceThumbnailMaxPixelSize: pixels * 2,
              ] as CFDictionary)
        else { return nil }
        let image = UIImage(cgImage: small)
        cache.setObject(image, forKey: name as NSString)
        return image
    }
}

/// The note's text, for the touch-and-hold preview (a tab note's page above
/// it).
struct NotePreview: View {
    @EnvironmentObject private var store: NotesStore
    let id: String

    var body: some View {
        let closed = store.core.isClosed(id: id)
        let editor = closed ? nil : store.core.openEditor(id: id)
        let text = editor.map { NoteTextRenderer.attributed($0.state().paragraphs, textSize: store.core.settings().textSize) }
        let tab = editor?.tabPage()
        Group {
            if let text, let tab {
                VStack(alignment: .leading, spacing: 0) {
                    VStack(alignment: .leading, spacing: 6) {
                        TabPicture(page: tab)
                            .frame(height: 160)
                        Text(tab.title)
                            .font(.headline)
                            .lineLimit(2)
                        Label(tab.shortUrl, systemImage: "link")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    .padding([.horizontal, .top], 16)
                    AttributedTextPreview(text: text)
                }
            } else if let text {
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
                } else if let tab = note.tab {
                    // A tab note: its page's picture (or site) is the card.
                    TabPicture(page: tab)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Color(.separator), lineWidth: 0.5))
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
