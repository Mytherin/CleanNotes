import SwiftUI

/// Settings, laid out like Notes' page in the Settings app: accounts, the
/// default account, viewing options, syncing and the locked notes password.
/// These are the same settings as the desktop app's.
struct SettingsView: View {
    @EnvironmentObject private var store: NotesStore
    @Environment(\.dismiss) private var dismiss
    @State private var setPassword = false
    @State private var useBiometrics = Biometrics.enabled
    @State private var askBiometricsPassword = false
    @State private var biometricsPassword = ""
    @State private var biometricsError: String?

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
                    NavigationLink("Add S3 Account") { S3AccountView(again: nil) }
                    if !overview.accounts.contains(where: { $0.id == localAccountId() }) {
                        Button("Add “On My iPhone”") { store.perform { $0.addLocalAccount() } }
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
                        if Biometrics.available {
                            Toggle("Use \(Biometrics.name)", isOn: Binding(
                                get: { useBiometrics },
                                set: { on in
                                    if on {
                                        biometricsPassword = ""
                                        askBiometricsPassword = true
                                    } else {
                                        Biometrics.turnOff()
                                        useBiometrics = false
                                    }
                                }
                            ))
                        }
                    } else {
                        Button("Set Password") { setPassword = true }
                    }
                    if store.core.hasUnlocked() {
                        Button("Lock Now") {
                            store.saveEditor()
                            store.perform { $0.lockNow() }
                            NoteImages.shared.forgetAll()
                        }
                    }
                } header: {
                    Text("Password")
                } footer: {
                    if useBiometrics {
                        Text("Locked notes are encrypted with this password, here and wherever they sync to. \(Biometrics.name) opens them on this iPhone: the password is kept in its keychain, readable only after \(Biometrics.name).")
                    } else {
                        Text("Locked notes are encrypted with this password, here and wherever they sync to.")
                    }
                }
            }
            .alert("Use \(Biometrics.name)", isPresented: $askBiometricsPassword) {
                SecureField("Password", text: $biometricsPassword)
                Button("Cancel", role: .cancel) { biometricsPassword = "" }
                Button("OK") { turnOnBiometrics() }
            } message: {
                Text("Enter the password for your locked notes.")
            }
            .alert("Couldn't Turn On \(Biometrics.name)", isPresented: Binding(get: { biometricsError != nil }, set: { if !$0 { biometricsError = nil } })) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(biometricsError ?? "")
            }
            .onAppear { useBiometrics = Biometrics.enabled }
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

    /// Checks the password, then keeps it for Face ID.
    private func turnOnBiometrics() {
        let password = biometricsPassword
        biometricsPassword = ""
        do {
            try store.core.enterPassword(password: password)
            if Biometrics.turnOn(password: password) {
                useBiometrics = true
            } else {
                biometricsError = "The password couldn't be saved in the keychain. Is a passcode set for this iPhone?"
            }
            store.changed()
        } catch {
            biometricsError = message(error)
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

/// An account: its state, Sync Now and Sign In Again (if synced), Remove.
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
                    if let s3 = a.s3 {
                        LabeledContent("Bucket", value: s3.bucket)
                        LabeledContent("Region", value: s3.region)
                        LabeledContent("Folder", value: s3.folder.isEmpty ? "Top of the Bucket" : s3.folder)
                        if let endpoint = s3.endpoint {
                            LabeledContent("Endpoint", value: endpoint)
                        }
                        LabeledContent("Encryption", value: s3.encrypted ? "On" : "Off")
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
                        if a.s3 != nil {
                            NavigationLink("Change Access Key") { S3AccountView(again: a) }
                        }
                    }
                }
                // Any account but the last can be removed, "On My iPhone"
                // too (its notes are deleted), as on the desktop.
                if a.synced || a.canRemove {
                    Section {
                        Button("Remove Account", role: .destructive) { confirmRemove = true }
                            .disabled(!a.canRemove)
                    } footer: {
                        Text(removeFooter(a))
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
            } message: {
                Text(a.synced ? "Its notes are removed from this iPhone. They stay where they're synced to." : localRemoveMessage(a))
            }
        } else {
            ContentUnavailableView("No Account", systemImage: "person.crop.circle.badge.questionmark")
        }
    }

    private func removeFooter(_ a: AccountItem) -> String {
        if !a.canRemove {
            return "The last account can't be removed."
        }
        return a.synced
            ? "Its notes are removed from this iPhone. They stay where they're synced to."
            : "Its notes are only on this iPhone and are deleted for good. It can be added back, empty, in Settings."
    }

    private func localRemoveMessage(_ a: AccountItem) -> String {
        let notes = a.noteCount == 1 ? "Its 1 note is" : "Its \(a.noteCount) notes are"
        return "\(notes) deleted for good: they're only on this iPhone. You can add the account back, empty, in Settings."
    }
}

/// Adding an S3 account (Amazon S3, or an S3-compatible service such as
/// Cloudflare R2 or MinIO), or giving one a new access key. The bucket is
/// checked before the account is added, as on the desktop.
struct S3AccountView: View {
    @EnvironmentObject private var store: NotesStore
    @Environment(\.dismiss) private var dismiss
    /// Giving this account a new access key (its bucket and folder fixed).
    let again: AccountItem?

    @State private var name = ""
    @State private var bucket = ""
    @State private var region = defaultS3Region()
    @State private var folder = defaultS3Folder()
    @State private var endpoint = ""
    @State private var accessKeyId = ""
    @State private var secretAccessKey = ""
    @State private var passphrase = ""
    @State private var encrypted = false
    @State private var busy = false
    @State private var error: String?

    private var ready: Bool {
        let e = endpoint.trimmingCharacters(in: .whitespaces)
        return !bucket.trimmingCharacters(in: .whitespaces).isEmpty
            && !accessKeyId.trimmingCharacters(in: .whitespaces).isEmpty
            && !secretAccessKey.trimmingCharacters(in: .whitespaces).isEmpty
            && (e.isEmpty || e.hasPrefix("http"))
            && (!encrypted || !passphrase.isEmpty)
    }

    var body: some View {
        Form {
            if again == nil {
                Section {
                    TextField("S3", text: $name, prompt: Text("Name (Optional)"))
                } footer: {
                    Text("Each note is a Markdown file in the folder's notes/ subfolder, so the desktop app and other tools can read it.")
                }
                Section("Bucket") {
                    TextField("Bucket", text: $bucket, prompt: Text("my-notes-bucket"))
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                    LabeledContent("Region") {
                        TextField("Region", text: $region, prompt: Text("us-east-1"))
                            .multilineTextAlignment(.trailing)
                            .textInputAutocapitalization(.never).autocorrectionDisabled()
                    }
                    LabeledContent("Folder") {
                        TextField("Folder", text: $folder, prompt: Text("Top of the Bucket"))
                            .multilineTextAlignment(.trailing)
                            .textInputAutocapitalization(.never).autocorrectionDisabled()
                    }
                }
                Section {
                    TextField("Endpoint", text: $endpoint, prompt: Text("Empty for Amazon S3"))
                        .keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                } header: {
                    Text("S3-Compatible Service")
                } footer: {
                    Text("For Cloudflare R2, MinIO and others, their address (R2's region is “auto”).")
                }
            }
            Section {
                TextField("Access Key ID", text: $accessKeyId, prompt: Text("AKIA…"))
                    .textInputAutocapitalization(.never).autocorrectionDisabled()
                SecureField("Secret Access Key", text: $secretAccessKey)
            } header: {
                Text("Access Key")
            } footer: {
                Text("Use an IAM user that can only list, read, write and delete in this bucket. The key is kept in the keychain.")
            }
            Section {
                SecureField(encrypted ? "Encryption Key" : "Encryption Key (Optional)", text: $passphrase)
            } footer: {
                Text(encrypted
                    ? "This folder's notes are encrypted: enter the key used on your other devices."
                    : "With a key, notes are encrypted on this iPhone before they're uploaded, and every device needs the same key. Without it they can't be read, so keep it safe. For a folder that's encrypted already, enter its key.")
            }
            if let error {
                Section { Text(error).foregroundStyle(.red) }
            }
            Section {
                Button {
                    check()
                } label: {
                    HStack {
                        Text(again == nil ? "Add Account" : "Save")
                        if busy { Spacer(); ProgressView() }
                    }
                }
                .disabled(busy || !ready)
            }
        }
        .disabled(busy)
        .navigationTitle(again == nil ? "Add S3 Account" : "Change Access Key")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear(perform: prefill)
    }

    private func prefill() {
        guard let a = again, let s3 = a.s3 else { return }
        name = a.name
        bucket = s3.bucket
        region = s3.region
        folder = s3.folder
        endpoint = s3.endpoint ?? ""
        encrypted = s3.encrypted
    }

    private func check() {
        busy = true
        error = nil
        let e = endpoint.trimmingCharacters(in: .whitespaces)
        let settings = S3Settings(bucket: bucket, region: region, folder: folder, endpoint: e.isEmpty ? nil : e, encrypted: encrypted)
        let (keyId, secret, passphrase) = (accessKeyId, secretAccessKey, self.passphrase)
        // The check talks to S3 (and making an encryption key takes a
        // moment): not on the main thread.
        Task.detached {
            let result: Result<S3Checked, Error>
            do {
                result = .success(try checkS3(settings: settings, accessKeyId: keyId, secretAccessKey: secret, passphrase: passphrase.isEmpty ? nil : passphrase))
            } catch {
                result = .failure(error)
            }
            await MainActor.run { finish(result) }
        }
    }

    private func finish(_ result: Result<S3Checked, Error>) {
        busy = false
        switch result {
        case let .failure(e):
            error = message(e)
        case let .success(checked):
            store.saveEditor()
            _ = store.core.addS3Account(again: again?.id, name: name, checked: checked)
            secretAccessKey = ""
            passphrase = ""
            store.changed()
            dismiss()
        }
    }
}
