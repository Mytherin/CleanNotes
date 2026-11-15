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
                    NavigationLink("Add Account") { AddAccountView(again: nil) }
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
                    if let email = a.email {
                        LabeledContent("Account", value: email)
                    }
                    if let server = a.server {
                        LabeledContent("Server", value: server)
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
                        if a.kind == "Standard Notes" || a.kind == "Bitwarden" {
                            NavigationLink("Sign In Again") { AddAccountView(again: a) }
                        }
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

/// Adding a sync account (or signing in to one again): Standard Notes or
/// Bitwarden, with their two-step codes when asked for.
struct AddAccountView: View {
    @EnvironmentObject private var store: NotesStore
    @Environment(\.dismiss) private var dismiss
    /// Signing in again to this account.
    let again: AccountItem?

    enum Service: String, CaseIterable, Identifiable {
        case standardNotes = "Standard Notes"
        case bitwarden = "Bitwarden"
        var id: String { rawValue }
    }

    enum BitwardenServer: String, CaseIterable, Identifiable {
        case com = "bitwarden.com"
        case eu = "bitwarden.eu"
        case selfHosted = "Self-hosted"
        var id: String { rawValue }
    }

    @State private var service: Service = .standardNotes
    @State private var name = ""
    @State private var email = ""
    @State private var password = ""
    @State private var server = ""
    @State private var bwServer: BitwardenServer = .com
    @State private var folder = "Notes"
    @State private var useApiKey = false
    @State private var clientId = ""
    @State private var clientSecret = ""
    @State private var code = ""
    @State private var needsCode = false
    @State private var emailCode = false
    @State private var remember = true
    @State private var newDeviceCode = ""
    @State private var needsNewDeviceCode = false
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        Form {
            if again == nil {
                Section {
                    Picker("Service", selection: $service) {
                        ForEach(Service.allCases) { Text($0.rawValue).tag($0) }
                    }
                    TextField(service.rawValue, text: $name, prompt: Text("Name (Optional)"))
                } footer: {
                    Text(service == .standardNotes
                        ? "Notes are end-to-end encrypted and show up in the Standard Notes apps. Folders are tags."
                        : "Notes go into your vault as secure notes, encrypted on this iPhone.")
                }
            }
            Section("Server") {
                if service == .bitwarden {
                    Picker("Server", selection: $bwServer) {
                        ForEach(BitwardenServer.allCases) { Text($0.rawValue).tag($0) }
                    }
                    if bwServer == .selfHosted {
                        TextField("https://vault.example.com", text: $server)
                            .keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                    }
                } else {
                    TextField("api.standardnotes.com", text: $server)
                        .keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                }
            }
            Section("Sign In") {
                TextField("Email", text: $email)
                    .keyboardType(.emailAddress).textContentType(.username)
                    .textInputAutocapitalization(.never).autocorrectionDisabled()
                SecureField(service == .bitwarden ? "Master Password" : "Password", text: $password)
                    .textContentType(.password)
                if service == .bitwarden {
                    Toggle("Use an API Key", isOn: $useApiKey)
                    if useApiKey {
                        TextField("client_id", text: $clientId).textInputAutocapitalization(.never).autocorrectionDisabled()
                        SecureField("client_secret", text: $clientSecret)
                    }
                    if again == nil {
                        TextField("Vault Folder", text: $folder)
                    }
                }
            }
            if needsCode {
                Section {
                    TextField(emailCode ? "Code from the Email" : "Authenticator Code", text: $code)
                        .keyboardType(.numberPad).textContentType(.oneTimeCode)
                    if service == .bitwarden {
                        Toggle("Remember This iPhone", isOn: $remember)
                    }
                } header: {
                    Text("Two-Step Login")
                }
            }
            if needsNewDeviceCode {
                Section {
                    TextField("Verification Code", text: $newDeviceCode)
                        .keyboardType(.numberPad).textContentType(.oneTimeCode)
                } footer: {
                    Text("Bitwarden emailed a code to confirm this device.")
                }
            }
            if let error {
                Section { Text(error).foregroundStyle(.red) }
            }
            Section {
                Button {
                    signIn()
                } label: {
                    HStack {
                        Text("Sign In")
                        if busy { Spacer(); ProgressView() }
                    }
                }
                .disabled(busy || email.isEmpty || password.isEmpty)
            } footer: {
                Text("Your password is never stored: signing in keeps only what's needed to sync, in the keychain. iCloud (CloudKit) and SQLite syncing are in the desktop app.")
            }
        }
        .navigationTitle(again == nil ? "Add Account" : "Sign In Again")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear(perform: prefill)
    }

    private func prefill() {
        guard let a = again else { return }
        service = a.kind == "Bitwarden" ? .bitwarden : .standardNotes
        name = a.name
        email = a.email ?? ""
        let s = a.server ?? ""
        if service == .bitwarden {
            if s.contains("bitwarden.eu") { bwServer = .eu } else if s.contains("bitwarden.com") || s.isEmpty { bwServer = .com } else { bwServer = .selfHosted; server = s }
        } else {
            server = s
        }
    }

    private var bitwardenServer: String {
        switch bwServer {
        case .com: return "https://vault.bitwarden.com"
        case .eu: return "https://vault.bitwarden.eu"
        case .selfHosted: return server
        }
    }

    private func signIn() {
        busy = true
        error = nil
        let service = self.service
        let (email, password, server, code) = (self.email, self.password, self.server, needsCode ? self.code : "")
        let (bwServer, folder, useKey, clientId, clientSecret) = (bitwardenServer, self.folder, useApiKey, self.clientId, self.clientSecret)
        let (emailCode, remember, deviceCode) = (self.emailCode, self.remember, needsNewDeviceCode ? newDeviceCode : "")
        // Signing in is slow (key derivation, the network): not on the main
        // thread.
        Task.detached {
            let result: Result<SignInResult, Error>
            do {
                switch service {
                case .standardNotes:
                    result = .success(try signInStandardNotes(server: server, email: email, password: password, code: code.isEmpty ? nil : code))
                case .bitwarden:
                    result = .success(try signInBitwarden(
                        server: bwServer, email: email, password: password, folder: folder,
                        apiClientId: useKey ? clientId : nil, apiClientSecret: useKey ? clientSecret : nil,
                        code: code.isEmpty ? nil : code, emailCode: emailCode, remember: remember,
                        newDeviceCode: deviceCode.isEmpty ? nil : deviceCode
                    ))
                }
            } catch {
                result = .failure(error)
            }
            await MainActor.run { finish(result) }
        }
    }

    private func finish(_ result: Result<SignInResult, Error>) {
        busy = false
        switch result {
        case let .failure(e):
            error = message(e)
        case let .success(.done(secret)):
            store.saveEditor()
            switch service {
            case .standardNotes:
                _ = store.core.addStandardNotesAccount(again: again?.id, name: name, server: server, email: email, secret: secret)
            case .bitwarden:
                _ = store.core.addBitwardenAccount(again: again?.id, name: name, server: bitwardenServer, email: email, folder: folder, secret: secret)
            }
            password = ""
            store.changed()
            dismiss()
        case .success(.needsCode):
            needsCode = true
            emailCode = false
        case let .success(.needsTwoFactor(authenticator, email)):
            needsCode = true
            emailCode = !authenticator && email
        case .success(.needsNewDeviceCode):
            needsNewDeviceCode = true
        }
    }
}
