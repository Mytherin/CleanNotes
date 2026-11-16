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

    var body: some View {
        let _ = store.revision
        let item = store.core.note(id: noteId, place: .allNotes)
        let deleted = store.core.isDeleted(id: noteId)
        Group {
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
        .toolbar { toolbar(item: item, deleted: deleted) }
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
        .onAppear { open() }
        .onDisappear { leave() }
        .onChange(of: store.revision) { _, _ in
            // Locked by Lock Now (or the core) meanwhile.
            if !closed && store.core.isClosed(id: noteId) {
                closed = true
                editor = nil
            }
        }
    }

    @ToolbarContentBuilder
    private func toolbar(item: NoteItem?, deleted: Bool) -> some ToolbarContent {
        ToolbarItemGroup(placement: .topBarTrailing) {
            if bridge.isEditing {
                Button {
                    bridge.command(.undo)
                } label: { Image(systemName: "arrow.uturn.backward.circle") }
                    .disabled(!(bridge.formatting?.canUndo ?? false))
                    .accessibilityLabel("Undo")
                Button {
                    bridge.command(.redo)
                } label: { Image(systemName: "arrow.uturn.forward.circle") }
                    .disabled(!(bridge.formatting?.canRedo ?? false))
                    .accessibilityLabel("Redo")
            }
            if editor != nil, !closed, let text = store.core.markdown(id: noteId) {
                ShareLink(item: text) {
                    Image(systemName: "square.and.arrow.up")
                }
            }
            if let item {
                noteMenu(item: item, deleted: deleted)
            }
            if bridge.isEditing {
                Button("Done") { bridge.endEditing() }
                    .bold()
            }
        }
        ToolbarItemGroup(placement: .bottomBar) {
            if !deleted && !closed {
                Button {
                    bridge.beginEditing()
                    bridge.command(.toggleChecklist)
                } label: { Image(systemName: "checklist") }
                    .accessibilityLabel("Checklist")
                Spacer()
                Button {
                    bridge.beginEditing()
                    bridge.toggleFormatPanel()
                } label: { Image(systemName: "textformat") }
                    .accessibilityLabel("Format")
                Spacer()
                Button {
                    newNote()
                } label: { Image(systemName: "square.and.pencil") }
                    .accessibilityLabel("New Note")
            }
        }
    }

    private func noteMenu(item: NoteItem, deleted: Bool) -> some View {
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
            Image(systemName: "ellipsis.circle")
        }
    }

    // ---- Opening and leaving ----

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
