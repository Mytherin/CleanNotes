import SwiftUI

/// A locked note's screen: "This note is locked." and View Note, which asks
/// for the password (one password opens every locked note, until Lock Now
/// or a while without use).
struct LockedNoteView: View {
    @EnvironmentObject private var store: NotesStore
    let noteId: String
    let onUnlocked: () -> Void

    @State private var asking = false
    @State private var password = ""
    @State private var error: String?
    @State private var triedBiometrics = false

    var body: some View {
        VStack(spacing: 14) {
            Spacer()
            Image(systemName: "lock.fill")
                .font(.system(size: 54))
                .foregroundStyle(.secondary)
            Text("This note is locked.")
                .font(.title3.bold())
            Text(Biometrics.ready ? "Use \(Biometrics.name) or enter the notes password to view locked notes." : "Enter the notes password to view locked notes.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            if let error {
                Text(error)
                    .font(.footnote)
                    .foregroundStyle(.red)
                    .multilineTextAlignment(.center)
            }
            // A plain yellow text button, as in Apple Notes (not a filled one).
            Button("View Note") { viewNote() }
                .font(.body.weight(.semibold))
                .buttonStyle(.borderless)
                .tint(Color.notesAccent)
                .padding(.top, 6)
            Spacer()
            Spacer()
        }
        .padding(32)
        .alert("Enter Password", isPresented: $asking) {
            SecureField("Password", text: $password)
            Button("Cancel", role: .cancel) {}
            Button("OK") { unlock(with: password, typed: true) }
        } message: {
            Text("Enter the password for your locked notes.")
        }
        .onAppear {
            // With Face ID on, it's asked for right away, as in Apple Notes.
            if !triedBiometrics && Biometrics.ready {
                triedBiometrics = true
                viewNote()
            }
        }
    }

    /// Face ID if it's on, else (or if it fails) the password.
    private func viewNote() {
        password = ""
        guard Biometrics.ready else {
            asking = true
            return
        }
        Biometrics.password(reason: "Unlock your locked notes.") { kept in
            if let kept {
                unlock(with: kept, typed: false)
            } else {
                asking = true
            }
        }
    }

    private func unlock(with password: String, typed: Bool) {
        do {
            try store.core.unlock(id: noteId, password: password)
            self.password = ""
            error = nil
            // A password typed while Face ID is on (say, after it was changed
            // on another device) is the one Face ID gives from now on.
            if typed { Biometrics.passwordChanged(to: password) }
            store.changed()
            onUnlocked()
        } catch {
            self.password = ""
            if typed {
                self.error = message(error)
            } else {
                // The kept password no longer opens the notes: ask for it.
                asking = true
            }
        }
    }
}

extension View {
    /// Locking a note: set `noteId` and the note is locked, asking first for
    /// a password (chosen the first time, entered once a session).
    func lockPrompts(noteId: Binding<String?>) -> some View {
        modifier(LockPrompts(noteId: noteId))
    }
}

private struct LockPrompts: ViewModifier {
    @EnvironmentObject private var store: NotesStore
    @Binding var noteId: String?
    @State private var choose: String?
    @State private var enter: String?
    @State private var password = ""
    @State private var failed: String?

    func body(content: Content) -> some View {
        content
            .onChange(of: noteId) { _, id in
                guard let id else { return }
                noteId = nil
                lock(id)
            }
            .sheet(isPresented: Binding(get: { choose != nil }, set: { if !$0 { choose = nil } })) {
                SetPasswordView { password, hint in
                    try store.core.setPassword(password: password, hint: hint)
                    if let id = choose {
                        choose = nil
                        lock(id)
                    }
                }
            }
            .alert("Enter Password", isPresented: Binding(get: { enter != nil }, set: { if !$0 { enter = nil } })) {
                SecureField("Password", text: $password)
                Button("Cancel", role: .cancel) { password = "" }
                Button("OK") {
                    let id = enter
                    do {
                        try store.core.enterPassword(password: password)
                        Biometrics.passwordChanged(to: password)
                        password = ""
                        enter = nil
                        if let id { lock(id) }
                    } catch {
                        password = ""
                        failed = message(error)
                    }
                }
            } message: {
                Text("Enter the password for your locked notes to lock this note.")
            }
            .alert("Couldn't Lock the Note", isPresented: Binding(get: { failed != nil }, set: { if !$0 { failed = nil } })) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(failed ?? "")
            }
    }

    private func lock(_ id: String) {
        store.saveEditor()
        switch store.core.lockNote(id: id) {
        case .locked: store.changed()
        case .choosePassword: choose = id
        case .enterPassword:
            guard Biometrics.ready else {
                enter = id
                return
            }
            Biometrics.password(reason: "Lock this note with your notes password.") { kept in
                if let kept, (try? store.core.enterPassword(password: kept)) != nil {
                    lock(id)
                } else {
                    enter = id
                }
            }
        }
    }
}

/// Choosing the password for locked notes (the first time a note is
/// locked, or from Settings).
struct SetPasswordView: View {
    @Environment(\.dismiss) private var dismiss
    let save: (String, String) throws -> Void

    @State private var password = ""
    @State private var verify = ""
    @State private var hint = ""
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    SecureField("Password", text: $password)
                    SecureField("Verify", text: $verify)
                } footer: {
                    Text("Locked notes are encrypted with this password. If you forget it, they can't be recovered.")
                }
                Section {
                    TextField("Hint (Recommended)", text: $hint)
                }
                if let error {
                    Section { Text(error).foregroundStyle(.red) }
                }
            }
            .navigationTitle("Set Password")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") {
                        guard password == verify else {
                            error = "The passwords don't match."
                            return
                        }
                        do {
                            try save(password, hint)
                            dismiss()
                        } catch {
                            self.error = message(error)
                        }
                    }
                    .disabled(password.isEmpty)
                }
            }
        }
    }
}

/// Settings → Change Password: every locked note gets the new password.
struct ChangePasswordView: View {
    @EnvironmentObject private var store: NotesStore
    @Environment(\.dismiss) private var dismiss

    @State private var old = ""
    @State private var password = ""
    @State private var verify = ""
    @State private var hint = ""
    @State private var error: String?

    var body: some View {
        Form {
            Section {
                SecureField("Old Password", text: $old)
            }
            Section {
                SecureField("New Password", text: $password)
                SecureField("Verify", text: $verify)
            }
            Section {
                TextField("Hint (Recommended)", text: $hint)
            }
            if let error {
                Section { Text(error).foregroundStyle(.red) }
            }
        }
        .navigationTitle("Change Password")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { hint = store.core.settings().lockHint }
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Done") {
                    guard password == verify else {
                        error = "The new passwords don't match."
                        return
                    }
                    do {
                        store.saveEditor()
                        _ = try store.core.changePassword(old: old, password: password, hint: hint)
                        Biometrics.passwordChanged(to: password)
                        store.changed()
                        dismiss()
                    } catch {
                        self.error = message(error)
                    }
                }
                .disabled(old.isEmpty || password.isEmpty)
            }
        }
    }
}

/// "Select a folder": where to move notes, in any account.
struct MoveSheet: View {
    @EnvironmentObject private var store: NotesStore
    let ids: [String]
    let done: () -> Void

    var body: some View {
        let overview = store.core.overview()
        NavigationStack {
            List {
                ForEach(overview.accounts) { account in
                    Section(account.name) {
                        ForEach(account.folders) { f in
                            Button {
                                store.saveEditor()
                                store.perform { $0.moveNotes(ids: ids, folder: f.id) }
                                done()
                            } label: {
                                Label(f.name, systemImage: "folder")
                                    .padding(.leading, CGFloat(f.depth) * 18)
                            }
                            .foregroundStyle(.primary)
                        }
                    }
                }
            }
            .navigationTitle(ids.count == 1 ? "Select a folder" : "Move \(ids.count) Notes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { done() } }
            }
        }
    }
}
