import SwiftUI

/// A note, as in Apple Notes: the text with its date on top; while typing,
/// undo and redo and Done in the navigation bar and the format bar above
/// the keyboard; share and the "…" menu (find, move, pin, lock, delete);
/// a lock screen for a locked note.
struct NoteView: View {
    @EnvironmentObject private var store: NotesStore
    @Environment(\.dismiss) private var dismiss
    let noteId: String
    let startEditing: Bool
    @Binding var path: [Route]

    @StateObject private var bridge = EditorBridge()
    @State private var editor: NoteEditor?
    @State private var closed = false
    @State private var gone = false
    @State private var moving = false
    @State private var lockRequest: String?
    @State private var confirmForever = false
    @State private var blockedAlert = false
    @State private var linkAlert = false
    @State private var linkURL = ""
    @State private var syncToken = UUID()
    @State private var showedSearch = false

    var body: some View {
        let _ = store.revision
        let item = store.core.note(id: noteId, place: .allNotes)
        let deleted = store.core.isDeleted(id: noteId)
        withToolbar(item: item, deleted: deleted) {
            if gone || item == nil {
                ContentUnavailableView("No Note", systemImage: "note.text", description: Text("This note was deleted."))
            } else if closed {
                LockedNoteView(noteId: noteId) { open() }
            } else if let editor {
                NoteEditorView(
                    editor: editor,
                    store: store,
                    bridge: bridge,
                    date: store.core.noteDate(id: noteId),
                    readOnly: deleted,
                    startEditing: startEditing,
                    onBlockedEdit: { blockedAlert = true },
                    onLink: {
                        linkURL = bridge.linkAtSelection()
                        linkAlert = true
                    }
                )
                .ignoresSafeArea(.container, edges: .bottom)
            } else {
                Color(.systemBackground)
            }
        }
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(bridge.isEditing ? .hidden : .visible, for: .bottomBar)
        .sheet(isPresented: $moving) {
            MoveSheet(ids: [noteId]) { moving = false }
        }
        .lockPrompts(noteId: $lockRequest)
        .alert("Recently Deleted notes can't be edited.", isPresented: $blockedAlert) {
            Button("Cancel", role: .cancel) {}
            Button("Recover") { store.perform { $0.recoverNote(id: noteId) } }
        } message: {
            Text("To edit this note, you'll need to recover it.")
        }
        .alert("Add Link", isPresented: $linkAlert) {
            TextField("URL", text: $linkURL)
                .keyboardType(.URL)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Button("Cancel", role: .cancel) {}
            if !bridge.linkAtSelection().isEmpty {
                Button("Remove Link", role: .destructive) { bridge.command(.setLink(url: nil)) }
            }
            Button("Done") { bridge.command(.setLink(url: linkURL)) }
        }
        .confirmationDialog("Delete this note?", isPresented: $confirmForever, titleVisibility: .visible) {
            Button("Delete", role: .destructive) {
                store.perform { $0.deleteForever(ids: [noteId]) }
                dismiss()
            }
        } message: {
            Text("This action can't be undone.")
        }
        .onAppear {
            open()
            showSearch()
        }
        .onDisappear { leave() }
        .onChange(of: store.revision) { _, _ in
            // Locked by Lock Now (or the core) meanwhile.
            if !closed && store.core.isClosed(id: noteId) {
                closed = true
                editor = nil
            }
        }
    }

    /// The note with its toolbars: iOS 26's look there, the earlier one
    /// before.
    @ViewBuilder
    private func withToolbar<Content: View>(item: NoteItem?, deleted: Bool, @ViewBuilder _ content: () -> Content) -> some View {
        if #available(iOS 26.0, *) {
            Group(content: content).toolbar { glassToolbar(item: item, deleted: deleted) }
        } else {
            Group(content: content).toolbar { toolbar(item: item, deleted: deleted) }
        }
    }

    /// iOS 26's look, as in Apple Notes: undo, then share and "…" in a
    /// capsule, then a yellow checkmark for Done while typing.
    @available(iOS 26.0, *)
    @ToolbarContentBuilder
    private func glassToolbar(item: NoteItem?, deleted: Bool) -> some ToolbarContent {
        // Undo stays after Done while there's something to undo, as in
        // Apple Notes.
        if bridge.isEditing || (bridge.formatting?.canUndo ?? false) {
            ToolbarItemGroup(placement: .topBarTrailing) {
                undoButtons(symbols: ("arrow.uturn.backward", "arrow.uturn.forward"))
                    .tint(Color.primary)
            }
            ToolbarSpacer(.fixed, placement: .topBarTrailing)
        }
        ToolbarItemGroup(placement: .topBarTrailing) {
            // White, as Apple Notes' (the checkmark is the yellow one).
            shareButton(item: item)
                .tint(Color.primary)
            if let item {
                noteMenu(item: item, deleted: deleted, symbol: "ellipsis")
                    .tint(Color.primary)
            }
        }
        if bridge.isEditing {
            ToolbarSpacer(.fixed, placement: .topBarTrailing)
            ToolbarItem(placement: .topBarTrailing) {
                Button(role: .confirm) {
                    bridge.endEditing()
                } label: { Image(systemName: "checkmark") }
                    .tint(Color.notesAccent)
                    .accessibilityLabel("Done")
            }
        }
        // As in Apple Notes: checklist, attach and table together, the
        // compose button on its own at the other end.
        if !deleted && !closed {
            ToolbarItemGroup(placement: .bottomBar) {
                checklistButton
                AttachMenu { kind in
                    bridge.beginEditing()
                    bridge.attach(kind)
                }
                .tint(Color.primary)
                tableMenu
            }
            ToolbarSpacer(.flexible, placement: .bottomBar)
            ToolbarItem(placement: .bottomBar) { composeButton.tint(Color.primary) }
        }
    }

    @ToolbarContentBuilder
    private func toolbar(item: NoteItem?, deleted: Bool) -> some ToolbarContent {
        ToolbarItemGroup(placement: .topBarTrailing) {
            if bridge.isEditing {
                undoButtons(symbols: ("arrow.uturn.backward.circle", "arrow.uturn.forward.circle"))
            }
            shareButton(item: item)
            if let item {
                noteMenu(item: item, deleted: deleted, symbol: "ellipsis.circle")
            }
            if bridge.isEditing {
                Button("Done") { bridge.endEditing() }
                    .bold()
            }
        }
        bottomBar(deleted: deleted)
    }

    /// Undo, and Redo once there's something to redo (as Apple Notes shows
    /// it).
    @ViewBuilder
    private func undoButtons(symbols: (undo: String, redo: String)) -> some View {
        Button {
            bridge.command(.undo)
        } label: { Image(systemName: symbols.undo) }
            .disabled(!(bridge.formatting?.canUndo ?? false))
            .accessibilityLabel("Undo")
        if bridge.formatting?.canRedo ?? false {
            Button {
                bridge.command(.redo)
            } label: { Image(systemName: symbols.redo) }
                .accessibilityLabel("Redo")
        }
    }

    @ViewBuilder
    private func shareButton(item: NoteItem?) -> some View {
        if let editor, !closed {
            let title = item?.title ?? "Note"
            ShareLink(
                item: SharedNote(title: title, textSize: store.core.settings().textSize, editor: editor),
                subject: Text(title),
                preview: SharePreview(title)
            ) {
                Image(systemName: "square.and.arrow.up")
            }
            .accessibilityLabel("Share")
        }
    }

    @ToolbarContentBuilder
    private func bottomBar(deleted: Bool) -> some ToolbarContent {
        ToolbarItemGroup(placement: .bottomBar) {
            if !deleted && !closed {
                checklistButton
                Spacer()
                AttachMenu { kind in
                    bridge.beginEditing()
                    bridge.attach(kind)
                }
                Spacer()
                tableMenu
                Spacer()
                composeButton
            }
        }
    }

    private var checklistButton: some View {
        Button {
            bridge.beginEditing()
            bridge.command(.toggleChecklist)
        } label: { Image(systemName: "checklist") }
            .accessibilityLabel("Checklist")
            .tint(Color.primary)
    }

    private var tableMenu: some View {
        Menu {
            TableMenu(formatting: bridge.formatting) { c in
                bridge.beginEditing()
                bridge.command(c)
            }
        } label: { Image(systemName: "tablecells") }
            .accessibilityLabel("Table")
            .tint(Color.primary)
    }

    private var composeButton: some View {
        Button {
            newNote()
        } label: { Image(systemName: "square.and.pencil") }
            .accessibilityLabel("New Note")
    }

    private func noteMenu(item: NoteItem, deleted: Bool, symbol: String) -> some View {
        Menu {
            if deleted {
                Button { store.perform { $0.recoverNote(id: noteId) } } label: { Label("Recover", systemImage: "arrow.uturn.backward") }
                Button { moving = true } label: { Label("Move Note", systemImage: "folder") }
                Button(role: .destructive) { confirmForever = true } label: { Label("Delete", systemImage: "trash") }
            } else {
                if !closed {
                    Button { bridge.find() } label: { Label("Find in Note", systemImage: "magnifyingglass") }
                }
                if !closed, let f = bridge.formatting, f.style == .checklist {
                    Menu {
                        Button { bridge.command(.checkAll) } label: { Label("Check All", systemImage: "checkmark.circle") }
                            .disabled(!f.checklistHasUnchecked)
                        Button { bridge.command(.uncheckAll) } label: { Label("Uncheck All", systemImage: "circle") }
                            .disabled(!f.checklistHasChecked)
                        Button { bridge.command(.moveCheckedToBottom) } label: { Label("Move Checked to Bottom", systemImage: "arrow.down.to.line") }
                            .disabled(!f.checklistHasChecked)
                        Button(role: .destructive) { bridge.command(.deleteChecked) } label: { Label("Delete Checked Items", systemImage: "trash") }
                            .disabled(!f.checklistHasChecked)
                    } label: {
                        Label("Checklist", systemImage: "checklist")
                    }
                }
                Button { moving = true } label: { Label("Move Note", systemImage: "folder") }
                Button {
                    store.perform { $0.togglePin(id: noteId) }
                } label: { Label(item.pinned ? "Unpin" : "Pin", systemImage: item.pinned ? "pin.slash" : "pin") }
                if item.locked {
                    if !closed {
                        Button {
                            store.saveEditor()
                            store.perform { $0.removeLock(id: noteId) }
                        } label: { Label("Remove", systemImage: "lock.open") }
                    }
                } else {
                    Button {
                        store.saveEditor()
                        lockRequest = noteId
                    } label: { Label("Lock", systemImage: "lock") }
                }
                if !closed && editor != nil {
                    Button { bridge.printNote(title: item.title) } label: { Label("Print", systemImage: "printer") }
                }
                Divider()
                Button(role: .destructive) { delete() } label: { Label("Delete", systemImage: "trash") }
            }
        } label: {
            Image(systemName: symbol)
        }
        .accessibilityLabel("More")
    }

    // ---- Opening and leaving ----

    /// Opened from search results: the note's matches are shown at once
    /// (Find in Note with the search's first word), as Apple Notes
    /// highlights them. Once, not on coming back to the note.
    private func showSearch() {
        guard !showedSearch, !startEditing, !closed, editor != nil else { return }
        showedSearch = true
        guard let word = store.search.split(whereSeparator: \.isWhitespace).first else { return }
        let query = String(word)
        // Once the text view is on screen.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) { bridge.find(query) }
    }

    /// Opens the note (again, coming back to it from a note pushed on top).
    private func open() {
        if !store.core.exists(id: noteId) {
            gone = true
            editor = nil
            return
        }
        if store.core.isClosed(id: noteId) {
            closed = true
            editor = nil
            return
        }
        closed = false
        if editor == nil {
            editor = startEditing ? store.core.newNoteEditor(id: noteId) : store.core.openEditor(id: noteId)
        }
        store.activeEditor = editor
        store.core.touchLock()
        store.onSyncChange[syncToken] = { followSync() }
    }

    /// A sync changed notes: this one follows (or closes).
    private func followSync() {
        guard let editor else { return }
        if !store.core.exists(id: noteId) {
            gone = true
            self.editor = nil
            return
        }
        if store.core.reloadEditor(editor: editor) {
            bridge.coordinator?.render()
        } else {
            self.editor = nil
            store.activeEditor = nil
            closed = store.core.isClosed(id: noteId)
        }
    }

    private func leave() {
        store.onSyncChange[syncToken] = nil
        if let editor {
            _ = store.core.saveEditor(editor: editor)
        }
        if store.activeEditor === editor {
            store.activeEditor = nil
        }
        // A new note left empty goes away, as in Apple Notes (once it's left,
        // not just covered by another note).
        let stillOpen = path.contains { route in
            if case let .note(id, _) = route { return id == noteId }
            return false
        }
        if !stillOpen {
            _ = store.core.leaveNote(id: noteId)
        }
        store.changed()
    }

    private func delete() {
        store.saveEditor()
        if store.core.deletesForever(ids: [noteId]) {
            confirmForever = true
            return
        }
        let place: Place
        if case let .list(p) = path.dropLast().last {
            place = p
        } else {
            place = .allNotes
        }
        store.perform { $0.trashNotes(ids: [noteId], place: place) }
        dismiss()
    }

    private func newNote() {
        store.saveEditor()
        var place = Place.allNotes
        if case let .list(p) = path.dropLast().last { place = p }
        let n = store.core.newNote(place: place)
        path.append(.note(id: n.id, isNew: true))
    }
}

/// The Aa panel, in the keyboard's place: paragraph styles, B I U S, lists,
/// indents, quote and monospaced.
struct FormatPanel: View {
    @ObservedObject var bridge: EditorBridge

    var body: some View {
        let f = bridge.formatting
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Text("Format").font(.headline)
                Spacer()
                Button {
                    bridge.toggleFormatPanel()
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.title2)
                        .symbolRenderingMode(.hierarchical)
                        .foregroundStyle(.secondary)
                }
                .accessibilityLabel("Close")
            }
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    style("Title", .title, font: .title3.bold(), f: f)
                    style("Heading", .heading, font: .headline, f: f)
                    style("Subheading", .subheading, font: .subheadline.weight(.semibold), f: f)
                    style("Body", .body, font: .body, f: f)
                    style("Monospaced", .monospaced, font: .body.monospaced(), f: f)
                }
            }
            HStack(spacing: 8) {
                group {
                    toggle("bold", on: f?.bold, .bold, label: "Bold")
                    toggle("italic", on: f?.italic, .italic, label: "Italic")
                    toggle("underline", on: f?.underline, .underline, label: "Underline")
                    toggle("strikethrough", on: f?.strike, .strikethrough, label: "Strikethrough")
                }
                group {
                    toggle("text.quote", on: f?.style == .quote, .setStyle(style: f?.style == .quote ? .body : .quote), label: "Block Quote")
                }
            }
            HStack(spacing: 8) {
                group {
                    toggle("list.bullet", on: f?.style == .bulleted, .setStyle(style: f?.style == .bulleted ? .body : .bulleted), label: "Bulleted List")
                    toggle("list.dash", on: f?.style == .dashed, .setStyle(style: f?.style == .dashed ? .body : .dashed), label: "Dashed List")
                    toggle("list.number", on: f?.style == .numbered, .setStyle(style: f?.style == .numbered ? .body : .numbered), label: "Numbered List")
                }
                group {
                    toggle("decrease.indent", on: false, .outdent, label: "Outdent")
                    toggle("increase.indent", on: false, .indent, label: "Indent")
                }
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Color(.secondarySystemBackground))
    }

    private func style(_ name: String, _ s: ParagraphStyle, font: Font, f: Formatting?) -> some View {
        let on = f?.style == s
        return Button {
            bridge.command(.setStyle(style: s))
        } label: {
            Text(name)
                .font(font)
                .lineLimit(1)
                .padding(.horizontal, 14)
                .frame(height: 40)
                .background(on ? Color.notesAccent : Color(.tertiarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
                .foregroundStyle(on ? Color.white : Color.primary)
        }
        .buttonStyle(.plain)
    }

    private func group<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        HStack(spacing: 0) { content() }
            .background(Color(.tertiarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
    }

    private func toggle(_ symbol: String, on: Bool?, _ command: EditorCommand, label: String) -> some View {
        let on = on ?? false
        return Button {
            bridge.command(command)
        } label: {
            Image(systemName: symbol)
                .font(.body.weight(.medium))
                .frame(maxWidth: .infinity, minHeight: 44)
                .background(on ? Color.notesAccent.opacity(0.9) : Color.clear, in: RoundedRectangle(cornerRadius: 10))
                .foregroundStyle(on ? Color.white : Color.primary)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}

/// The note's Table menu (the bottom toolbar): Insert Table, or in a
/// table, the next cell, rows, columns, headers and deleting.
struct TableMenu: View {
    let formatting: Formatting?
    let run: (EditorCommand) -> Void

    var body: some View {
        if let f = formatting, f.inTable {
            Section {
                Button { run(.nextCell) } label: { Label("Next Cell", systemImage: "arrow.right.to.line") }
                Button { run(.previousCell) } label: { Label("Previous Cell", systemImage: "arrow.left.to.line") }
            }
            Section {
                Button { run(.addRowAbove) } label: { Label("Add Row Above", systemImage: "arrow.up.to.line") }
                Button { run(.addRowBelow) } label: { Label("Add Row Below", systemImage: "arrow.down.to.line") }
                Button { run(.addColumnBefore) } label: { Label("Add Column Before", systemImage: "arrow.left.to.line.compact") }
                Button { run(.addColumnAfter) } label: { Label("Add Column After", systemImage: "arrow.right.to.line.compact") }
            }
            Section {
                Toggle(isOn: Binding(get: { f.headerRow }, set: { _ in run(.toggleHeaderRow) })) {
                    Label("Header Row", systemImage: "rectangle.topthird.inset.filled")
                }
                Toggle(isOn: Binding(get: { f.headerColumn }, set: { _ in run(.toggleHeaderColumn) })) {
                    Label("Header Column", systemImage: "rectangle.leftthird.inset.filled")
                }
            }
            Section {
                Button(role: .destructive) { run(.deleteRow) } label: { Label("Delete Row", systemImage: "minus.square") }
                Button(role: .destructive) { run(.deleteColumn) } label: { Label("Delete Column", systemImage: "minus.square") }
                Button(role: .destructive) { run(.deleteTable) } label: { Label("Delete Table", systemImage: "trash") }
            }
        } else {
            Button { run(.insertTable) } label: { Label("Insert Table", systemImage: "tablecells") }
        }
    }
}
