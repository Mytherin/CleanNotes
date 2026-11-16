import SwiftUI

/// The Folders screen, as in Apple Notes: each account's folders (nested,
/// with note counts), Recently Deleted, tags; search across all notes; new
/// folder and new note in the bottom toolbar.
struct FoldersView: View {
    @EnvironmentObject private var store: NotesStore
    @Binding var path: [Route]

    @State private var search = ""
    @State private var collapsed: Set<String> = []
    @State private var folderAlert: FolderAlert?
    @State private var folderName = ""
    @State private var chooseAccount = false
    @State private var deleting: FolderItem?
    @State private var showSettings = false

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
        .refreshable { await store.refresh() }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button { showSettings = true } label: { Label("Settings", systemImage: "gear") }
                    if store.core.hasUnlocked() {
                        Button {
                            store.saveEditor()
                            store.perform { $0.lockNow() }
                        } label: { Label("Lock Now", systemImage: "lock") }
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
            ToolbarItemGroup(placement: .bottomBar) {
                Button {
                    if overview.accounts.count > 1 {
                        chooseAccount = true
                    } else {
                        newFolder(account: overview.accounts.first?.id ?? localAccountId(), parent: nil)
                    }
                } label: {
                    Image(systemName: "folder.badge.plus")
                }
                .accessibilityLabel("New Folder")
                Spacer()
                Button {
                    let n = store.core.newNote(place: .folder(id: defaultFolder(overview)))
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
                            if !f.isDefault {
                                Button(role: .destructive) { deleting = f } label: { Label("Delete", systemImage: "trash") }
                            }
                        }
                        .contextMenu { folderMenu(f, account: account) }
                    }
                    if account.deletedCount > 0 {
                        NavigationLink(value: Route.list(.deleted(account: account.id))) {
                            FolderRow(name: "Recently Deleted", icon: "trash", count: overview.showNoteCount ? Int(account.deletedCount) : nil)
                        }
                    }
                } header: {
                    AccountHeader(account: account)
                }
            }
            if !overview.tags.isEmpty {
                Section("Tags") {
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
    private func folderMenu(_ f: FolderItem, account: AccountItem) -> some View {
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
            Divider()
            Button(role: .destructive) { deleting = f } label: { Label("Delete", systemImage: "trash") }
        }
    }

    private func defaultFolder(_ overview: Overview) -> String {
        let settings = store.core.settings()
        let account = overview.accounts.first { $0.id == settings.defaultAccount } ?? overview.accounts.first
        return account?.folders.first { $0.isDefault }?.id ?? ""
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
        let notes = store.core.sections(place: .allNotes, search: search).flatMap(\.notes)
        List {
            Section(notes.isEmpty ? "No Results" : "Top Hits") {
                ForEach(notes) { n in
                    NavigationLink(value: Route.note(id: n.id, isNew: false)) {
                        NoteRow(note: n)
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
    }
}
