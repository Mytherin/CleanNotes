import SwiftUI

/// The Folders screen, as in Apple Notes: each account's folders (nested,
/// with note counts), Recently Deleted, tags; search across all notes; new
/// folder and new note in the bottom toolbar.
struct FoldersView: View {
    @EnvironmentObject private var store: NotesStore
    @ObservedObject private var passwords = PasswordsStore.shared
    @Binding var path: [Route]

    @State private var search = ""
    @State private var collapsed: Set<String> = []
    @State private var folderAlert: FolderAlert?
    @State private var folderName = ""
    @State private var chooseAccount = false
    @State private var deleting: FolderItem?
    @State private var showSettings = false
    @State private var smartEditor: SmartEditing?
    @State private var deletingSmart: SmartFolderItem?

    var body: some View {
        let _ = store.revision
        let overview = store.core.overview()
        Group {
            if search.trimmingCharacters(in: .whitespaces).isEmpty {
                folders(overview)
            } else {
                SearchResults(search: search, path: $path)
            }
        }
        .navigationTitle("Folders")
        .searchable(text: $search, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search")
        .onChange(of: search) { _, s in store.search = s }
        .onAppear { store.search = search }
        .refreshable { await store.refresh() }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button { showSettings = true } label: { Label("Settings", systemImage: "gear") }
                    // Lock Now locks CleanPassword's vault too, as on the desktop.
                    if store.core.hasUnlocked() || passwords.core.status().unlocked {
                        Button {
                            store.saveEditor()
                            store.perform { $0.lockNow() }
                            NoteImages.shared.forgetAll()
                            passwords.lock()
                        } label: { Label("Lock Now", systemImage: "lock") }
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
            ToolbarItemGroup(placement: .bottomBar) {
                Menu {
                    Button {
                        if overview.accounts.count > 1 {
                            chooseAccount = true
                        } else {
                            newFolder(account: overview.accounts.first?.id ?? localAccountId(), parent: nil)
                        }
                    } label: { Label("New Folder", systemImage: "folder") }
                    Button { smartEditor = .new } label: { Label("New Smart Folder", systemImage: "gearshape") }
                } label: {
                    Image(systemName: "folder.badge.plus")
                }
                .accessibilityLabel("New Folder")
                Spacer()
                Button {
                    let n = store.core.newNote(place: .folder(id: store.core.defaultFolder()))
                    path = [.list(n.place), .note(id: n.id, isNew: true)]
                } label: {
                    Image(systemName: "square.and.pencil")
                }
                .accessibilityLabel("New Note")
            }
        }
        .confirmationDialog("New Folder", isPresented: $chooseAccount, titleVisibility: .visible) {
            ForEach(overview.accounts) { a in
                Button(a.name) { newFolder(account: a.id, parent: nil) }
            }
        } message: {
            Text("Choose where the new folder goes.")
        }
        .alert(folderAlert?.title ?? "", isPresented: Binding(get: { folderAlert != nil }, set: { if !$0 { folderAlert = nil } })) {
            TextField("Name", text: $folderName)
            Button("Cancel", role: .cancel) { folderAlert = nil }
            Button("Save") { saveFolder() }
                .disabled(folderName.trimmingCharacters(in: .whitespaces).isEmpty)
        } message: {
            Text("Enter a name for this folder.")
        }
        .confirmationDialog(
            "Delete Folder?",
            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
            titleVisibility: .visible
        ) {
            Button("Delete Folder", role: .destructive) {
                if let f = deleting {
                    store.saveEditor()
                    store.perform { $0.deleteFolder(id: f.id) }
                }
                deleting = nil
            }
        } message: {
            Text("All notes and subfolders in this folder will be deleted. Notes will be kept in Recently Deleted for 30 days.")
        }
        .sheet(isPresented: $showSettings) { SettingsView() }
        .sheet(item: $smartEditor) { editing in
            SmartFolderEditor(existing: editing.folder, tags: overview.tags) { id in
                if editing.folder == nil {
                    path.append(.list(.smart(id: id)))
                }
            }
        }
        .confirmationDialog(
            "Delete Smart Folder?",
            isPresented: Binding(get: { deletingSmart != nil }, set: { if !$0 { deletingSmart = nil } }),
            titleVisibility: .visible
        ) {
            Button("Delete Smart Folder", role: .destructive) {
                if let s = deletingSmart {
                    store.perform { $0.deleteSmartFolder(id: s.id) }
                }
                deletingSmart = nil
            }
        } message: {
            Text("Its notes stay where they are.")
        }
    }

    @ViewBuilder
    private func folders(_ overview: Overview) -> some View {
        List {
            if overview.showAllNotes {
                Section {
                    NavigationLink(value: Route.list(.allNotes)) {
                        FolderRow(name: "All Notes", icon: "tray.full", count: overview.showNoteCount ? Int(overview.allCount) : nil)
                    }
                }
            }
            ForEach(overview.accounts) { account in
                Section(isExpanded: Binding(
                    get: { !collapsed.contains(account.id) },
                    set: { open in if open { collapsed.remove(account.id) } else { collapsed.insert(account.id) } }
                )) {
                    ForEach(visibleFolders(account)) { f in
                        NavigationLink(value: Route.list(.folder(id: f.id))) {
                            FolderRow(name: f.name, icon: "folder", count: overview.showNoteCount ? Int(f.count) : nil)
                                .padding(.leading, CGFloat(f.depth) * 18)
                        }
                        .swipeActions(edge: .trailing) {
                            Button(role: .destructive) { deleting = f } label: { Label("Delete", systemImage: "trash") }
                        }
                        .contextMenu { folderMenu(f, account: account, accounts: overview.accounts) }
                        .folderDrag(f)
                        .dropDestination(for: String.self) { items, _ in
                            dropFolders(items, account: account.id, parent: f.id)
                        }
                    }
                    if account.deletedCount > 0 {
                        NavigationLink(value: Route.list(.deleted(account: account.id))) {
                            FolderRow(name: "Recently Deleted", icon: "trash", count: overview.showNoteCount ? Int(account.deletedCount) : nil)
                        }
                    }
                } header: {
                    // A folder dropped on the account goes to its top level.
                    AccountHeader(account: account)
                        .dropDestination(for: String.self) { items, _ in
                            dropFolders(items, account: account.id, parent: nil)
                        }
                }
            }
            // CleanPassword's vault, when an S3 account's bucket holds one.
            PasswordsSection(collapsed: $collapsed)
            // CleanTabs' saved tabs, when an S3 account's folder has them.
            TabsSection(tabs: overview.tabs, showCount: overview.showNoteCount, collapsed: $collapsed)
            if !overview.smartFolders.isEmpty {
                Section("Smart Folders") {
                    ForEach(overview.smartFolders) { s in
                        NavigationLink(value: Route.list(.smart(id: s.id))) {
                            FolderRow(name: s.name, icon: "gearshape", count: overview.showNoteCount ? Int(s.count) : nil)
                        }
                        .swipeActions(edge: .trailing) {
                            Button(role: .destructive) { deletingSmart = s } label: { Label("Delete", systemImage: "trash") }
                        }
                        .contextMenu {
                            Button { smartEditor = .edit(s) } label: { Label("Edit Smart Folder", systemImage: "slider.horizontal.3") }
                            Divider()
                            Button(role: .destructive) { deletingSmart = s } label: { Label("Delete", systemImage: "trash") }
                        }
                    }
                }
            }
            if !overview.tags.isEmpty {
                Section("Tags", isExpanded: Binding(
                    get: { !collapsed.contains("#tags") },
                    set: { open in if open { collapsed.remove("#tags") } else { collapsed.insert("#tags") } }
                )) {
                    TagCloud(tags: overview.tags) { tag in path.append(.list(.tag(name: tag))) }
                }
            }
        }
        .listStyle(.sidebar)
    }

    /// Folders, without those inside a collapsed one.
    private func visibleFolders(_ account: AccountItem) -> [FolderItem] {
        var hidden: Set<String> = []
        var out: [FolderItem] = []
        for f in account.folders {
            if let p = f.parent, hidden.contains(p) || collapsed.contains(p) {
                hidden.insert(f.id)
                continue
            }
            out.append(f)
        }
        return out
    }

    @ViewBuilder
    private func folderMenu(_ f: FolderItem, account: AccountItem, accounts: [AccountItem]) -> some View {
        if !f.isDefault {
            Button {
                folderName = f.name
                folderAlert = .rename(f.id)
            } label: { Label("Rename", systemImage: "pencil") }
        }
        Button {
            newFolder(account: account.id, parent: f.id)
        } label: { Label("New Subfolder", systemImage: "folder.badge.plus") }
        if f.hasChildren {
            Button {
                if collapsed.contains(f.id) { collapsed.remove(f.id) } else { collapsed.insert(f.id) }
            } label: {
                Label(collapsed.contains(f.id) ? "Show Subfolders" : "Hide Subfolders", systemImage: "chevron.down")
            }
        }
        if !f.isDefault {
            Menu {
                ForEach(accounts) { a in
                    Section(a.name) {
                        Button("Top Level") { moveFolder(f.id, account: a.id, parent: nil) }
                            .disabled(!store.core.canMoveFolder(id: f.id, account: a.id, parent: nil))
                        ForEach(a.folders) { t in
                            Button(String(repeating: "\u{2003}", count: Int(t.depth)) + t.name) {
                                moveFolder(f.id, account: a.id, parent: t.id)
                            }
                            .disabled(!store.core.canMoveFolder(id: f.id, account: a.id, parent: t.id))
                        }
                    }
                }
            } label: { Label("Move", systemImage: "folder") }
        }
        Divider()
        Button(role: .destructive) { deleting = f } label: { Label("Delete", systemImage: "trash") }
    }

    /// Folders dragged onto a folder (or onto an account: `parent` nil) move
    /// into it.
    private func dropFolders(_ items: [String], account: String, parent: String?) -> Bool {
        var moved = false
        for item in items where item.hasPrefix(folderDragPrefix) {
            moved = moveFolder(String(item.dropFirst(folderDragPrefix.count)), account: account, parent: parent) || moved
        }
        return moved
    }

    @discardableResult
    private func moveFolder(_ id: String, account: String, parent: String?) -> Bool {
        guard store.core.moveFolder(id: id, account: account, parent: parent) else { return false }
        // Where it went opens up to show it.
        collapsed.remove(parent ?? account)
        store.changed()
        return true
    }

    private func newFolder(account: String, parent: String?) {
        folderName = "New Folder"
        folderAlert = .new(account: account, parent: parent)
    }

    private func saveFolder() {
        let name = folderName.trimmingCharacters(in: .whitespaces)
        switch folderAlert {
        case let .new(account, parent):
            let id = store.core.newFolder(account: account, parent: parent, name: name)
            store.changed()
            path.append(.list(.folder(id: id)))
        case let .rename(id):
            do {
                try store.core.renameFolder(id: id, name: name)
                store.changed()
            } catch {
                store.show(message(error))
            }
        case nil:
            break
        }
        folderAlert = nil
    }
}

/// Making a Smart Folder, or changing one.
private enum SmartEditing: Identifiable {
    case new
    case edit(SmartFolderItem)

    var id: String { folder?.id ?? "new" }

    var folder: SmartFolderItem? {
        if case let .edit(s) = self { return s }
        return nil
    }
}

/// A Smart Folder's name and filters, as Apple Notes' New Smart Folder
/// sheet: tags (any or all of them), checklists, pinned, locked, and when
/// notes were edited.
struct SmartFolderEditor: View {
    @EnvironmentObject private var store: NotesStore
    @Environment(\.dismiss) private var dismiss
    let existing: SmartFolderItem?
    let tags: [String]
    /// Saved (with its id).
    let saved: (String) -> Void

    @State private var name: String
    @State private var chosenTags: Set<String>
    @State private var allTags: Bool
    @State private var checklists: ChecklistFilter
    @State private var pinned: Bool
    @State private var locked: Bool
    @State private var days: UInt32

    init(existing: SmartFolderItem?, tags: [String], saved: @escaping (String) -> Void) {
        self.existing = existing
        self.tags = tags
        self.saved = saved
        let f = existing?.filter
        _name = State(initialValue: existing?.name ?? "")
        _chosenTags = State(initialValue: Set(f?.tags ?? []))
        _allTags = State(initialValue: f?.allTags ?? false)
        _checklists = State(initialValue: f?.checklists ?? .off)
        _pinned = State(initialValue: f?.pinned ?? false)
        _locked = State(initialValue: f?.locked ?? false)
        _days = State(initialValue: f?.editedWithinDays ?? 0)
    }

    var body: some View {
        // Tags in the notes, and any chosen before that no note has now.
        let choices = (tags + chosenTags.filter { !tags.contains($0) }.sorted())
        NavigationStack {
            Form {
                Section {
                    TextField("Name", text: $name)
                }
                Section {
                    if choices.isEmpty {
                        Text("Add #tags to notes to filter by them.").foregroundStyle(.secondary)
                    }
                    ForEach(choices, id: \.self) { t in
                        Button {
                            if chosenTags.contains(t) { chosenTags.remove(t) } else { chosenTags.insert(t) }
                        } label: {
                            HStack {
                                Text("#\(t)").foregroundStyle(.primary)
                                Spacer()
                                if chosenTags.contains(t) {
                                    Image(systemName: "checkmark").foregroundStyle(Color.notesAccent)
                                }
                            }
                        }
                    }
                    if chosenTags.count > 1 {
                        Picker("Include Notes With", selection: $allTags) {
                            Text("Any Selected Tags").tag(false)
                            Text("All Selected Tags").tag(true)
                        }
                    }
                } header: {
                    Text("Tags")
                }
                Section {
                    Picker("Checklists", selection: $checklists) {
                        Text("Off").tag(ChecklistFilter.off)
                        Text("Any Checklist").tag(ChecklistFilter.hasChecklist)
                        Text("Unchecked Items").tag(ChecklistFilter.unchecked)
                        Text("All Items Checked").tag(ChecklistFilter.allChecked)
                    }
                    Toggle("Pinned Notes", isOn: $pinned)
                    Toggle("Locked Notes", isOn: $locked)
                    Picker("Edited", selection: $days) {
                        Text("Any Time").tag(UInt32(0))
                        Text("Today").tag(UInt32(1))
                        Text("In the Last 7 Days").tag(UInt32(7))
                        Text("In the Last 30 Days").tag(UInt32(30))
                    }
                } header: {
                    Text("Filters")
                } footer: {
                    Text("Notes from every folder that pass every filter that's on.")
                }
            }
            .navigationTitle(existing == nil ? "New Smart Folder" : "Edit Smart Folder")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { save() }
                        .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
        }
    }

    private func save() {
        let filter = SmartFilter(
            tags: chosenTags.sorted(),
            allTags: allTags,
            checklists: checklists,
            pinned: pinned,
            locked: locked,
            editedWithinDays: days
        )
        let id: String
        if let existing {
            store.core.updateSmartFolder(id: existing.id, name: name, filter: filter)
            id = existing.id
        } else {
            id = store.core.addSmartFolder(name: name, filter: filter)
        }
        store.changed()
        dismiss()
        saved(id)
    }
}

/// What a dragged folder carries: its id, marked so text dragged in from
/// elsewhere isn't taken for one.
private let folderDragPrefix = "cleannotes-folder:"

private extension View {
    /// Folders other than "Notes" can be dragged into other folders.
    @ViewBuilder
    func folderDrag(_ f: FolderItem) -> some View {
        if f.isDefault {
            self
        } else {
            draggable(folderDragPrefix + f.id) {
                FolderRow(name: f.name, icon: "folder", count: nil)
            }
        }
    }
}

private enum FolderAlert {
    case new(account: String, parent: String?)
    case rename(String)

    var title: String {
        switch self {
        case .new: return "New Folder"
        case .rename: return "Rename Folder"
        }
    }
}

struct FolderRow: View {
    let name: String
    let icon: String
    let count: Int?

    var body: some View {
        HStack {
            Label {
                Text(name)
            } icon: {
                Image(systemName: icon).foregroundStyle(Color.notesAccent)
            }
            Spacer()
            if let count {
                Text("\(count)").foregroundStyle(.secondary)
            }
        }
    }
}

/// An account's name, with its sync state.
private struct AccountHeader: View {
    @EnvironmentObject private var store: NotesStore
    let account: AccountItem
    @State private var showProblem = false

    var body: some View {
        HStack(spacing: 6) {
            Text(account.name)
            if account.syncing {
                ProgressView().controlSize(.mini)
            } else if account.needsSignIn || account.problem != nil {
                Button {
                    showProblem = true
                } label: {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                }
                .buttonStyle(.plain)
            }
        }
        .alert(account.needsSignIn ? "Sign In Again" : "Couldn't Sync", isPresented: $showProblem) {
            if !account.needsSignIn {
                Button("Try Again") { store.perform { $0.syncNow(account: account.id) } }
            }
            Button("OK", role: .cancel) {}
        } message: {
            Text(account.needsSignIn ? "The saved sign-in for “\(account.name)” stopped working. Sign in again in Settings → Accounts." : (account.problem ?? ""))
        }
    }
}

/// Tags as buttons, wrapping like Apple Notes' tag browser.
struct TagCloud: View {
    let tags: [String]
    let open: (String) -> Void

    var body: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 90), spacing: 8, alignment: .leading)], alignment: .leading, spacing: 8) {
            ForEach(tags, id: \.self) { tag in
                Button {
                    open(tag)
                } label: {
                    Text("#\(tag)")
                        .lineLimit(1)
                        .font(.subheadline)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 6)
                        .background(Color(.tertiarySystemFill), in: Capsule())
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.vertical, 4)
    }
}

/// Search results across every note, from the Folders screen.
struct SearchResults: View {
    @EnvironmentObject private var store: NotesStore
    let search: String
    @Binding var path: [Route]

    var body: some View {
        let _ = store.revision
        // Top Hits (titles that match), then Notes, as in Apple Notes.
        let sections = store.core.sections(place: .allNotes, search: search)
        Group {
            if sections.isEmpty {
                ContentUnavailableView.search(text: search)
            } else {
                List {
                    ForEach(sections, id: \.title) { section in
                        Section {
                            ForEach(section.notes) { n in
                                NavigationLink(value: Route.note(id: n.id, isNew: false)) {
                                    NoteRow(note: n)
                                }
                            }
                        } header: {
                            Text(section.title)
                                .font(.title3.bold())
                                .foregroundStyle(.primary)
                                .textCase(nil)
                        }
                    }
                }
                .listStyle(.insetGrouped)
            }
        }
    }
}
