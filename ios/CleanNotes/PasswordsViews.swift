import SwiftUI

// CleanPassword's vault, as in the desktop app: a section on the Folders
// screen (All Passwords, Codes, the vault's folders, Recently Deleted), the
// lock screen, and the lists of items, which look like Apple's Passwords
// app. Read-only but for deleting and recovering: items are edited in
// CleanPassword. The details are in PasswordDetailView.swift.

/// The CleanPassword section of the Folders screen, when an S3 account's
/// bucket holds a vault. Its rows' counts (and its folders, Codes and
/// Recently Deleted) show once it's unlocked.
struct PasswordsSection: View {
    @ObservedObject private var passwords = PasswordsStore.shared
    /// The Folders screen's folded sections ("#cleanpassword" for this one).
    @Binding var collapsed: Set<String>

    var body: some View {
        let _ = passwords.revision
        let status = passwords.core.status()
        if status.found {
            Section(isExpanded: Binding(
                get: { !collapsed.contains("#cleanpassword") },
                set: { open in if open { collapsed.remove("#cleanpassword") } else { collapsed.insert("#cleanpassword") } }
            )) {
                NavigationLink(value: Route.passwords(.all)) {
                    FolderRow(name: "All Passwords", icon: "key", count: status.unlocked ? Int(status.allCount) : nil)
                }
                if status.unlocked {
                    if status.codesCount > 0 {
                        NavigationLink(value: Route.passwords(.codes)) {
                            FolderRow(name: "Codes", icon: "lock.rotation", count: Int(status.codesCount))
                        }
                    }
                    ForEach(status.folders, id: \.path) { f in
                        NavigationLink(value: Route.passwords(.folder(path: f.path))) {
                            FolderRow(name: f.name, icon: "folder", count: Int(f.count))
                                .padding(.leading, CGFloat(f.depth) * 18)
                        }
                    }
                    if status.deletedCount > 0 {
                        NavigationLink(value: Route.passwords(.deleted)) {
                            FolderRow(name: "Recently Deleted", icon: "trash", count: Int(status.deletedCount))
                        }
                    }
                }
            } header: {
                PasswordsHeader(status: status)
            }
        }
    }
}

/// "CleanPassword", with a lock while it's locked, a spinner while it
/// unlocks or syncs, and a warning when a sync failed.
private struct PasswordsHeader: View {
    let status: VaultStatus
    @State private var showProblem = false

    var body: some View {
        HStack(spacing: 6) {
            Text("CleanPassword")
            if status.busy != nil || status.saving {
                ProgressView().controlSize(.mini)
            } else if status.unlocked && status.error != nil {
                Button {
                    showProblem = true
                } label: {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                }
                .buttonStyle(.plain)
            } else if !status.unlocked {
                Image(systemName: "lock.fill")
                    .imageScale(.small)
                    .foregroundStyle(.secondary)
            }
        }
        .alert("CleanPassword", isPresented: $showProblem) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(status.error ?? "")
        }
    }
}

/// "CleanPassword is locked.": the master password unlocks the vault (on
/// a background thread; it takes a moment). Shown in place of any vault
/// screen while it's locked.
struct VaultUnlockView: View {
    @ObservedObject private var passwords = PasswordsStore.shared
    @State private var password = ""
    @FocusState private var focused: Bool

    var body: some View {
        let _ = passwords.revision
        let status = passwords.core.status()
        let unlocking = status.busy == .unlocking
        if !status.found {
            ContentUnavailableView("No Vault", systemImage: "key", description: Text("No S3 account's bucket holds a CleanPassword vault any more."))
        } else {
            VStack(spacing: 14) {
                Spacer()
                Image(systemName: "key.fill")
                    .font(.system(size: 54))
                    .foregroundStyle(.secondary)
                Text("CleanPassword is locked.")
                    .font(.title3.bold())
                Text("Enter the master password to view your passwords.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                SecureField("Master Password", text: $password)
                    .textContentType(.password)
                    .submitLabel(.go)
                    .padding(12)
                    .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
                    .focused($focused)
                    .disabled(unlocking)
                    .onSubmit { unlock() }
                if unlocking {
                    ProgressView("Unlocking…")
                        .padding(.top, 6)
                } else {
                    if let error = status.error {
                        Text(error)
                            .font(.footnote)
                            .foregroundStyle(.red)
                            .multilineTextAlignment(.center)
                    }
                    // A plain yellow text button, as View Note on a locked note.
                    Button("Unlock") { unlock() }
                        .font(.body.weight(.semibold))
                        .buttonStyle(.borderless)
                        .tint(Color.notesAccent)
                        .disabled(password.isEmpty)
                        .padding(.top, 6)
                }
                Spacer()
                Text("Your CleanPassword vault, in \(status.place).")
                    .font(.footnote)
                    .foregroundStyle(.tertiary)
                    .multilineTextAlignment(.center)
            }
            .padding(32)
            .onAppear { focused = true }
        }
    }

    private func unlock() {
        guard !password.isEmpty else { return }
        passwords.core.unlock(password: password)
        password = ""
        passwords.changed()
    }
}

/// A CleanPassword list (All Passwords, Codes, a folder, Recently Deleted),
/// as in Apple's Passwords app: each item's badge, name and user name (in
/// Codes, its code ticking down); search; swipe left to delete, and in
/// Recently Deleted right to recover; touch and hold to copy; Select to
/// delete or recover several. Locked, it's the lock screen.
struct PasswordListView: View {
    @ObservedObject private var passwords = PasswordsStore.shared
    let place: VaultPlace

    @State private var search = ""
    @State private var editMode: EditMode = .inactive
    @State private var selection: Set<String> = []
    @State private var confirmForever: [String]?

    private var deletedView: Bool { place == .deleted }

    var body: some View {
        let _ = passwords.revision
        let status = passwords.core.status()
        let rows = status.unlocked ? passwords.core.items(place: place, search: search) : []
        Group {
            if status.unlocked {
                list(rows, status: status)
            } else {
                VaultUnlockView()
            }
        }
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.large)
        .environment(\.editMode, $editMode)
        .toolbar { toolbar(rows, status: status) }
        .vaultCover()
        .confirmationDialog(
            (confirmForever?.count ?? 0) == 1 ? "Delete this item permanently?" : "Delete \(confirmForever?.count ?? 0) items permanently?",
            isPresented: Binding(get: { confirmForever != nil }, set: { if !$0 { confirmForever = nil } }),
            titleVisibility: .visible
        ) {
            Button("Delete", role: .destructive) {
                if let ids = confirmForever {
                    passwords.deletePermanently(ids)
                }
                confirmForever = nil
                endSelecting()
            }
        } message: {
            Text("It's deleted from CleanPassword on every device. This action can't be undone.")
        }
        .onAppear { passwords.touch() }
    }

    private var title: String {
        switch place {
        case .all: return "All Passwords"
        case .codes: return "Codes"
        case let .folder(path): return path.split(separator: "/").last.map(String.init) ?? path
        case .deleted: return "Recently Deleted"
        }
    }

    private func list(_ rows: [VaultRow], status: VaultStatus) -> some View {
        List(selection: editMode.isEditing ? $selection : nil) {
            Section {
                ForEach(rows) { row in
                    NavigationLink(value: Route.password(id: row.id)) {
                        PasswordRow(row: row, showCode: place == .codes)
                    }
                    .tag(row.id)
                    .swipeActions(edge: .leading, allowsFullSwipe: true) {
                        if deletedView {
                            Button {
                                passwords.recover([row.id])
                            } label: { Label("Recover", systemImage: "arrow.uturn.backward") }
                                .tint(.blue)
                        }
                    }
                    .swipeActions(edge: .trailing, allowsFullSwipe: !deletedView) {
                        Button(role: deletedView ? nil : .destructive) {
                            delete([row.id])
                        } label: { Label("Delete", systemImage: "trash") }
                            .tint(.red)
                    }
                    .contextMenu { menu(row) }
                }
            } footer: {
                if let error = status.error {
                    Text(error).foregroundStyle(.red)
                } else if deletedView && !rows.isEmpty {
                    Text("Items are available here for 30 days. After that time, CleanPassword deletes them permanently.")
                }
            }
        }
        .listStyle(.insetGrouped)
        .searchable(text: $search, prompt: "Search")
        .onChange(of: search) { _, _ in passwords.touch() }
        .refreshable { await passwords.refresh() }
        .overlay {
            if rows.isEmpty {
                if search.trimmingCharacters(in: .whitespaces).isEmpty {
                    ContentUnavailableView("No Items", systemImage: deletedView ? "trash" : "key", description: Text(deletedView ? "Items deleted in CleanPassword or here stay here for 30 days." : "Add passwords in CleanPassword."))
                } else {
                    ContentUnavailableView.search(text: search)
                }
            }
        }
    }

    /// Touch and hold: copy, open the website, delete (or recover).
    @ViewBuilder
    private func menu(_ row: VaultRow) -> some View {
        if deletedView {
            Button { passwords.recover([row.id]) } label: { Label("Recover", systemImage: "arrow.uturn.backward") }
            Button(role: .destructive) { confirmForever = [row.id] } label: { Label("Delete Permanently", systemImage: "trash") }
        } else {
            if !row.username.isEmpty {
                Button {
                    passwords.copy(row.username, what: "the user name", secret: false)
                } label: { Label("Copy User Name", systemImage: "person") }
            }
            if row.hasPassword {
                Button {
                    if let item = passwords.core.item(id: row.id) {
                        passwords.copy(item.password, what: "the password", secret: true)
                    }
                } label: { Label("Copy Password", systemImage: "key") }
            }
            if row.code != nil {
                Button {
                    if let code = passwords.core.code(id: row.id) {
                        passwords.copy(code.code, what: "the verification code", secret: false)
                    }
                } label: { Label("Copy Verification Code", systemImage: "lock.rotation") }
            }
            if let website = row.website, let url = URL(string: website) {
                Divider()
                Link(destination: url) { Label("Open Website", systemImage: "safari") }
            }
            Divider()
            Button(role: .destructive) { delete([row.id]) } label: { Label("Delete", systemImage: "trash") }
        }
    }

    @ToolbarContentBuilder
    private func toolbar(_ rows: [VaultRow], status: VaultStatus) -> some ToolbarContent {
        ToolbarItem(placement: .topBarTrailing) {
            if !status.unlocked {
                EmptyView()
            } else if editMode.isEditing {
                Button("Done") { endSelecting() }
                    .bold()
            } else {
                Menu {
                    Button {
                        editMode = .active
                    } label: { Label("Select Items", systemImage: "checkmark.circle") }
                        .disabled(rows.isEmpty)
                    Button {
                        Task { await passwords.refresh() }
                    } label: { Label("Refresh", systemImage: "arrow.clockwise") }
                    Button {
                        passwords.lock()
                    } label: { Label("Lock CleanPassword", systemImage: "lock") }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
        }
        ToolbarItemGroup(placement: .bottomBar) {
            if status.unlocked {
                if editMode.isEditing {
                    let shown = rows.map(\.id)
                    let chosen = shown.filter { selection.contains($0) }
                    if deletedView {
                        let ids = chosen.isEmpty ? shown : chosen
                        Button(chosen.isEmpty ? "Recover All" : "Recover") {
                            passwords.recover(ids)
                            endSelecting()
                        }
                        .disabled(ids.isEmpty)
                        Spacer()
                        Button(chosen.isEmpty ? "Delete All" : "Delete") { confirmForever = ids }
                            .disabled(ids.isEmpty)
                    } else {
                        Spacer()
                        Button("Delete") { delete(chosen) }
                            .disabled(chosen.isEmpty)
                    }
                } else {
                    Spacer()
                    Text(itemsCount(rows.count))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                }
            }
        }
    }

    /// Delete: to CleanPassword's Recently Deleted (from there, for good,
    /// after asking).
    private func delete(_ ids: [String]) {
        guard !ids.isEmpty else { return }
        if deletedView {
            confirmForever = ids
            return
        }
        passwords.delete(ids)
        endSelecting()
    }

    private func endSelecting() {
        selection = []
        editMode = .inactive
    }
}

/// "1 Item", "12 Items".
private func itemsCount(_ n: Int) -> String {
    n == 1 ? "1 Item" : "\(n) Items"
}

/// An item in a CleanPassword list: its badge, name and user name (in
/// Codes, its code ticking down).
struct PasswordRow: View {
    let row: VaultRow
    var showCode = false

    var body: some View {
        HStack(spacing: 12) {
            VaultBadge(title: row.title, note: row.kind == .note)
            VStack(alignment: .leading, spacing: 1) {
                Text(row.title)
                    .lineLimit(1)
                if !row.subtitle.isEmpty {
                    Text(row.subtitle)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            if showCode {
                Spacer()
                LiveCode(id: row.id)
            }
        }
    }
}

/// An item's verification code now, with a ring for the seconds it has
/// left, ticking.
struct LiveCode: View {
    let id: String
    var font: Font = .title3

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { _ in
            if let code = PasswordsStore.shared.core.code(id: id) {
                HStack(spacing: 10) {
                    Text(code.spaced)
                        .font(font.monospacedDigit())
                        .foregroundStyle(.primary)
                    CountdownRing(remaining: code.remaining, period: code.period)
                }
            }
        }
    }
}

/// The seconds a code has left, as a closing ring (red for the last five).
struct CountdownRing: View {
    let remaining: UInt32
    let period: UInt32

    var body: some View {
        ZStack {
            Circle().stroke(Color(.systemFill), lineWidth: 3)
            Circle()
                .trim(from: 0, to: CGFloat(remaining) / CGFloat(max(period, 1)))
                .stroke(remaining <= 5 ? Color.red : Color.notesAccent, style: StrokeStyle(lineWidth: 3, lineCap: .round))
                .rotationEffect(.degrees(-90))
        }
        .frame(width: 18, height: 18)
        .accessibilityLabel("\(remaining) seconds left")
    }
}

/// An item's badge: its first letter (a secure note's icon) on a circle in
/// a color of its own, the same as the desktop app's.
struct VaultBadge: View {
    let title: String
    let note: Bool
    var size: CGFloat = 32

    private static let colors: [Color] = [
        Color(red: 52 / 255, green: 139 / 255, blue: 246 / 255),
        Color(red: 48 / 255, green: 176 / 255, blue: 90 / 255),
        Color(red: 242 / 255, green: 140 / 255, blue: 40 / 255),
        Color(red: 175 / 255, green: 82 / 255, blue: 222 / 255),
        Color(red: 229 / 255, green: 72 / 255, blue: 92 / 255),
        Color(red: 90 / 255, green: 107 / 255, blue: 125 / 255),
    ]

    var body: some View {
        let n = title.utf8.reduce(UInt(0)) { $0 &* 31 &+ UInt($1) }
        ZStack {
            Circle().fill(Self.colors[Int(n % UInt(Self.colors.count))])
            if note {
                Image(systemName: "note.text")
                    .font(.system(size: size * 0.45, weight: .semibold))
            } else {
                Text(String(title.prefix(1)).uppercased())
                    .font(.system(size: size * 0.47, weight: .semibold))
            }
        }
        .foregroundStyle(.white)
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

extension View {
    /// Hides a vault screen behind a lock while the app isn't in front, so
    /// the app switcher doesn't show passwords (as CleanPassword does).
    func vaultCover() -> some View {
        modifier(VaultCover())
    }
}

private struct VaultCover: ViewModifier {
    @Environment(\.scenePhase) private var phase

    func body(content: Content) -> some View {
        content.overlay {
            if phase != .active {
                ZStack {
                    Rectangle().fill(.regularMaterial)
                    Image(systemName: "lock.fill")
                        .font(.largeTitle)
                        .foregroundStyle(.secondary)
                }
                .ignoresSafeArea()
            }
        }
    }
}
