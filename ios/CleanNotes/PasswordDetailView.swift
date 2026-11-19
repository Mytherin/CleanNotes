import SwiftUI

/// A CleanPassword item, as in Apple's Passwords app: its badge, name and
/// when it was modified on top; then user name, password (dots until
/// tapped) and verification code with its countdown; websites, passkeys,
/// fields, notes and earlier passwords, each with a copy button (a copied
/// password leaves the clipboard after 90 seconds). Read-only: items are
/// edited in CleanPassword; Delete moves one to its Recently Deleted.
struct PasswordDetailView: View {
    @ObservedObject private var passwords = PasswordsStore.shared
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL
    let id: String

    /// What's shown in the clear ("password", "field 2", "history 0").
    @State private var revealed: Set<String> = []
    /// The copy button just tapped, showing a checkmark.
    @State private var copied: String?
    @State private var confirmForever = false

    var body: some View {
        let _ = passwords.revision
        let status = passwords.core.status()
        Group {
            if !status.unlocked {
                VaultUnlockView()
            } else if let item = passwords.core.item(id: id) {
                details(item, status: status)
            } else {
                ContentUnavailableView("No Item", systemImage: "key", description: Text("It was deleted, here or on another device."))
            }
        }
        .navigationBarTitleDisplayMode(.inline)
        .vaultCover()
        .onAppear { passwords.touch() }
        .onChange(of: status.unlocked) { _, unlocked in
            // Locked: nothing stays shown in the clear.
            if !unlocked { revealed = [] }
        }
        .confirmationDialog("Delete this item permanently?", isPresented: $confirmForever, titleVisibility: .visible) {
            Button("Delete", role: .destructive) {
                passwords.deletePermanently([id])
                dismiss()
            }
        } message: {
            Text("It's deleted from CleanPassword on every device. This action can't be undone.")
        }
    }

    private func details(_ item: VaultItem, status: VaultStatus) -> some View {
        List {
            Section {
                VStack(spacing: 8) {
                    VaultBadge(title: item.title, note: item.kind == .note, size: 64)
                    Text(item.title)
                        .font(.title2.bold())
                        .multilineTextAlignment(.center)
                    Text("Last modified \(item.modified)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if item.deleted {
                        Text("This item is in CleanPassword's Recently Deleted.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    if let error = status.error {
                        Text(error)
                            .font(.footnote)
                            .foregroundStyle(.red)
                            .multilineTextAlignment(.center)
                    }
                }
                .frame(maxWidth: .infinity)
                .listRowBackground(Color.clear)
            }
            if !item.username.isEmpty || !item.password.isEmpty || item.code != nil {
                Section {
                    if !item.username.isEmpty {
                        copyRow("User Name", item.username, key: "user", what: "the user name")
                    }
                    if !item.password.isEmpty {
                        secretRow("Password", item.password, key: "password", what: "the password")
                    }
                    if item.code != nil {
                        codeRow()
                    }
                }
            }
            if !item.websites.isEmpty {
                Section {
                    ForEach(Array(item.websites.enumerated()), id: \.offset) { _, w in
                        Button(w.label) {
                            passwords.touch()
                            if let url = URL(string: w.url) { openURL(url) }
                        }
                        .contextMenu {
                            Button {
                                passwords.copy(w.uri, what: "the website", secret: false)
                            } label: { Label("Copy Website", systemImage: "doc.on.doc") }
                        }
                    }
                } header: {
                    Text(item.websites.count == 1 ? "Website" : "Websites")
                }
            }
            if !item.passkeys.isEmpty {
                Section("Passkeys") {
                    ForEach(Array(item.passkeys.enumerated()), id: \.offset) { _, k in
                        HStack(spacing: 12) {
                            Image(systemName: "person.badge.key.fill")
                                .foregroundStyle(Color.notesAccent)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(k.user.isEmpty ? k.site : k.user)
                                if !k.user.isEmpty {
                                    Text(k.site)
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                            }
                        }
                    }
                }
            }
            if !item.fields.isEmpty {
                Section("Fields") {
                    ForEach(Array(item.fields.enumerated()), id: \.offset) { n, f in
                        if f.hidden {
                            secretRow(f.name, f.value, key: "field \(n)", what: "the field")
                        } else {
                            copyRow(f.name, f.value, key: "field \(n)", what: "the field")
                        }
                    }
                }
            }
            if !item.notes.isEmpty {
                Section("Notes") {
                    Text(item.notes)
                        .textSelection(.enabled)
                }
            }
            if !item.history.isEmpty {
                Section {
                    DisclosureGroup("Earlier Passwords (\(item.history.count))") {
                        ForEach(Array(item.history.enumerated()), id: \.offset) { n, h in
                            copyRow("Until \(h.retired)", h.password, key: "history \(n)", what: "the earlier password", secret: true, mono: true)
                        }
                    }
                }
            }
            Section {
                if item.deleted {
                    Button("Recover") {
                        passwords.recover([item.id])
                        dismiss()
                    }
                    Button("Delete Permanently", role: .destructive) { confirmForever = true }
                } else {
                    Button(item.kind == .note ? "Delete Note" : "Delete Password", role: .destructive) {
                        passwords.delete([item.id])
                        dismiss()
                    }
                }
            } footer: {
                Text("From your CleanPassword vault, in \(status.place). Edit items in CleanPassword; delete them here or there.")
            }
        }
        .listStyle(.insetGrouped)
    }

    /// A value with its label, and a copy button.
    private func copyRow(_ label: String, _ value: String, key: String, what: String, secret: Bool = false, mono: Bool = false) -> some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(label)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text(value)
                    .font(mono ? .body.monospaced() : .body)
                    .textSelection(.enabled)
            }
            Spacer()
            copyButton(key: key) { passwords.copy(value, what: what, secret: secret) }
        }
        .contextMenu {
            Button {
                passwords.copy(value, what: what, secret: secret)
            } label: { Label("Copy \(label)", systemImage: "doc.on.doc") }
        }
    }

    /// A password or hidden field: dots until tapped, and a copy button.
    private func secretRow(_ label: String, _ value: String, key: String, what: String) -> some View {
        let shown = revealed.contains(key)
        return HStack {
            Button {
                toggle(key)
            } label: {
                VStack(alignment: .leading, spacing: 2) {
                    Text(label)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text(shown ? value : String(repeating: "•", count: 10))
                        .font(.body.monospaced())
                        .foregroundStyle(.primary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            copyButton(key: key) { passwords.copy(value, what: what, secret: true) }
        }
        .contextMenu {
            Button {
                passwords.copy(value, what: what, secret: true)
            } label: { Label("Copy \(label)", systemImage: "doc.on.doc") }
            Button {
                toggle(key)
            } label: { Label(shown ? "Hide \(label)" : "Show \(label)", systemImage: shown ? "eye.slash" : "eye") }
        }
    }

    /// The verification code now, ticking down.
    private func codeRow() -> some View {
        TimelineView(.periodic(from: .now, by: 1)) { _ in
            let code = passwords.core.code(id: id)
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Verification Code")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text(code?.spaced ?? "—")
                        .font(.title3.monospacedDigit())
                }
                Spacer()
                if let code {
                    CountdownRing(remaining: code.remaining, period: code.period)
                    copyButton(key: "code") { passwords.copy(code.code, what: "the verification code", secret: false) }
                }
            }
        }
    }

    private func copyButton(key: String, copy: @escaping () -> Void) -> some View {
        Button {
            copy()
            copied = key
            Task { @MainActor in
                try? await Task.sleep(for: .seconds(1.5))
                if copied == key { copied = nil }
            }
        } label: {
            Image(systemName: copied == key ? "checkmark" : "doc.on.doc")
        }
        .buttonStyle(.borderless)
        .accessibilityLabel("Copy")
    }

    private func toggle(_ key: String) {
        if revealed.contains(key) {
            revealed.remove(key)
        } else {
            revealed.insert(key)
        }
        passwords.touch()
    }
}
