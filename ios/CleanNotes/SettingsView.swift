import SwiftUI

/// Settings, laid out like Notes' page in the Settings app: accounts, the
/// default account, viewing options, syncing and the locked notes password.
/// These are the same settings as the desktop app's.
struct SettingsView: View {
    @EnvironmentObject private var store: NotesStore
    @Environment(\.dismiss) private var dismiss
    @State private var setPassword = false

    var body: some View {
        let _ = store.revision
        let settings = store.core.settings()
        let overview = store.core.overview()
        NavigationStack {
            Form {
                Section("Accounts") {
                    ForEach(overview.accounts) { a in
                        NavigationLink {
                            AccountDetailView(accountId: a.id)
                        } label: {
                            LabeledContent(a.name, value: a.synced ? a.kind : "")
                        }
                    }
                }
                if overview.accounts.count > 1 {
                    Section {
                        Picker("Default Account", selection: binding(settings, \.defaultAccount)) {
                            ForEach(overview.accounts) { a in Text(a.name).tag(a.id) }
                        }
                    } footer: {
                        Text("Notes made without choosing a folder go to this account.")
                    }
                }
                Section("Viewing") {
                    Picker("Sort Notes By", selection: binding(settings, \.sortBy)) {
                        Text("Date Edited").tag(SortOrder.dateEdited)
                        Text("Date Created").tag(SortOrder.dateCreated)
                        Text("Title").tag(SortOrder.title)
                    }
                    Picker("New Notes Start With", selection: binding(settings, \.newNotesStartWith)) {
                        Text("Title").tag(ParagraphStyle.title)
                        Text("Heading").tag(ParagraphStyle.heading)
                        Text("Subheading").tag(ParagraphStyle.subheading)
                        Text("Body").tag(ParagraphStyle.body)
                    }
                    Picker("Sort Checked Items", selection: binding(settings, \.sortChecked)) {
                        Text("Manually").tag(false)
                        Text("Automatically").tag(true)
                    }
                    Picker("Text Size", selection: binding(settings, \.textSize)) {
                        ForEach(textSizes(), id: \.self) { s in
                            Text(s == 15 ? "\(Int(NoteTextRenderer.bodySize(s))) pt (Default)" : "\(Int(NoteTextRenderer.bodySize(s))) pt").tag(s)
                        }
                    }
                    Toggle("Group Notes By Date", isOn: binding(settings, \.groupByDate))
                    Toggle("Show Note Count", isOn: binding(settings, \.showNoteCount))
                }
                Section {
                    Picker("Check for Changes", selection: binding(settings, \.refreshMinutes)) {
                        Text("Every Minute").tag(UInt32(1))
                        Text("Every 5 Minutes").tag(UInt32(5))
                        Text("Every 15 Minutes").tag(UInt32(15))
                        Text("Every 30 Minutes").tag(UInt32(30))
                        Text("Every Hour").tag(UInt32(60))
                        Text("Manually").tag(UInt32(0))
                    }
                } header: {
                    Text("Syncing")
                } footer: {
                    Text("Changes made here are sent right away. Pull down on a list to check for changes now.")
                }
                Section {
                    if settings.hasLockPassword {
                        NavigationLink("Change Password") { ChangePasswordView() }
                    } else {
                        Button("Set Password") { setPassword = true }
                    }
                    if store.core.hasUnlocked() {
                        Button("Lock Now") {
                            store.saveEditor()
                            store.perform { $0.lockNow() }
                        }
                    }
                } header: {
                    Text("Password")
                } footer: {
                    Text("Locked notes are encrypted with this password, here and wherever they sync to.")
                }
            }
            .navigationTitle("Notes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
            .sheet(isPresented: $setPassword) {
                SetPasswordView { password, hint in
                    try store.core.setPassword(password: password, hint: hint)
                    store.changed()
                }
            }
        }
    }

    /// A binding to one setting, saved as it changes.
    private func binding<T>(_ settings: AppSettings, _ key: WritableKeyPath<AppSettings, T>) -> Binding<T> {
        Binding(
            get: { settings[keyPath: key] },
            set: { value in
                var s = store.core.settings()
                s[keyPath: key] = value
                store.perform { $0.setSettings(new: s) }
            }
        )
    }
}

/// A synced account: its state, Sync Now, Sign In Again, Remove.
struct AccountDetailView: View {
    @EnvironmentObject private var store: NotesStore
    @Environment(\.dismiss) private var dismiss
    let accountId: String
    @State private var confirmRemove = false

    var body: some View {
        let _ = store.revision
        if let a = store.core.overview().accounts.first(where: { $0.id == accountId }) {
            Form {
                Section {
                    LabeledContent("Name", value: a.name)
                    if a.synced {
                        LabeledContent("Service", value: a.kind)
                    }
                }
                if a.synced {
                    Section {
                        if a.syncing {
                            HStack {
                                Text("Syncing…")
                                Spacer()
                                ProgressView()
                            }
                        } else if a.needsSignIn {
                            Text("The saved sign-in stopped working. Sign in again to keep syncing.")
                                .foregroundStyle(.orange)
                        } else if let problem = a.problem {
                            Text("Couldn't sync: \(problem)").foregroundStyle(.orange)
                        } else {
                            Text("Up to date").foregroundStyle(.secondary)
                        }
                        Button("Sync Now") { store.perform { $0.syncNow(account: a.id) } }
                            .disabled(a.syncing)
                    }
                    Section {
                        Button("Remove Account", role: .destructive) { confirmRemove = true }
                    } footer: {
                        Text("Its notes are removed from this iPhone. They stay where they're synced to.")
                    }
                }
            }
            .navigationTitle(a.name)
            .confirmationDialog("Remove the account “\(a.name)”?", isPresented: $confirmRemove, titleVisibility: .visible) {
                Button("Remove", role: .destructive) {
                    store.saveEditor()
                    store.perform { $0.removeAccount(id: a.id) }
                    dismiss()
                }
            }
        } else {
            ContentUnavailableView("No Account", systemImage: "person.crop.circle.badge.questionmark")
        }
    }
}
