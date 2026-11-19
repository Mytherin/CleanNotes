import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// CleanPassword's vault, when an S3 account's bucket holds one: found,
/// unlocked, synced and changed by the Rust core (`PasswordsCore`, the same
/// code as the desktop app's CleanPassword section), and shown in the
/// CleanPassword section of the Folders screen.
///
/// Screens read straight from `core` while they draw; `revision` goes up
/// whenever the vault changed (found or gone, unlocked, synced, a change
/// saved), which redraws them.
final class PasswordsStore: ObservableObject {
    static let shared = PasswordsStore()

    /// The vault locks when the app was in the background this long (and,
    /// while it's open, after 15 minutes without use: the core's).
    static let backgroundLock: TimeInterval = 5 * 60
    /// A copied password leaves the clipboard after this long, as in
    /// CleanPassword.
    static let clipboardLifetime: TimeInterval = 90

    let core: PasswordsCore
    /// Goes up when the vault changes.
    @Published private(set) var revision = 0

    private var timer: Timer?
    private var backgroundedAt: Date?

    /// The vault is kept in `directory` between unlocks, sealed with its key
    /// (Application Support/CleanNotes, with the notes).
    init(directory: URL? = nil) {
        let listener = VaultListener()
        core = PasswordsCore(dir: (directory ?? NotesStore.dataDirectory()).path, listener: listener)
        listener.store = self
    }

    // ---- Over time ----

    /// The app came to the foreground: locked if it was away for long, then
    /// looked for and synced as it runs.
    func becameActive() {
        if let t = backgroundedAt, Date().timeIntervalSince(t) >= Self.backgroundLock {
            lock()
        }
        backgroundedAt = nil
        tick()
        timer?.invalidate()
        // Fires on the main run loop.
        timer = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { [weak self] _ in
            self?.tick()
        }
    }

    /// The app went to the background (or is about to).
    func resignedActive() {
        if backgroundedAt == nil {
            backgroundedAt = Date()
        }
        timer?.invalidate()
        timer = nil
    }

    /// Looks for the vault when the S3 accounts changed (and every 10
    /// minutes), locks it after 15 minutes without use, and syncs it every
    /// minute.
    func tick() {
        let notes = NotesStore.shared
        guard notes.startupError == nil else { return }
        core.lookForVault(notes: notes.core)
        if core.tick() {
            changed()
        }
    }

    /// Something changed: screens redraw.
    func changed() {
        revision &+= 1
        // A typed master password unlocked the vault (or didn't): Face ID
        // keeps it, or offers to.
        if let typed = typedPassword {
            let s = core.status()
            if s.busy == nil {
                typedPassword = nil
                if s.unlocked && VaultFaceID.unlocked(with: typed) {
                    offerFaceID = typed
                }
            }
        }
    }

    /// The master password just typed, until the vault says whether it
    /// unlocks it.
    private var typedPassword: String?
    /// A master password that just unlocked the vault, while asking whether
    /// Face ID should keep it.
    @Published var offerFaceID: String?

    // ---- What the screens do ----

    /// Unlocks the vault with its master password, typed or kept for Face
    /// ID (in the background: `status().busy` until it's done).
    func unlock(password: String, typed: Bool) {
        core.unlock(password: password)
        typedPassword = typed ? password : nil
        changed()
    }

    /// Face ID unlocks the vault, when it's on: asks for it and unlocks with
    /// the kept master password. Returns false (and does nothing) when it's
    /// off.
    @discardableResult
    func unlockWithFaceID() -> Bool {
        guard VaultFaceID.kept.ready, core.status().busy == nil else { return false }
        VaultFaceID.kept.password(reason: "Unlock CleanPassword.") { [weak self] kept in
            guard let self, let kept, !self.core.status().unlocked else { return }
            self.unlock(password: kept, typed: false)
        }
        return true
    }

    /// The vault was used: it stays unlocked another 15 minutes.
    func touch() {
        core.touch()
    }

    /// Lock Now (also the Folders screen's, with the locked notes).
    func lock() {
        core.lock()
        changed()
    }

    /// Pull to refresh: sync the vault, and wait (a while) for it. (On the
    /// main actor: an async method would otherwise run elsewhere.)
    @MainActor
    func refresh() async {
        core.refresh()
        changed()
        for _ in 0..<40 {
            try? await Task.sleep(for: .milliseconds(250))
            if core.status().busy == nil { break }
        }
        changed()
    }

    /// To CleanPassword's Recently Deleted (shown at once, saved in the
    /// background).
    func delete(_ ids: [String]) {
        guard !ids.isEmpty else { return }
        core.delete(ids: ids)
        changed()
    }

    func recover(_ ids: [String]) {
        guard !ids.isEmpty else { return }
        core.recover(ids: ids)
        changed()
    }

    /// For good, from every device.
    func deletePermanently(_ ids: [String]) {
        guard !ids.isEmpty else { return }
        core.deletePermanently(ids: ids)
        changed()
    }

    /// Copies from the vault, to this iPhone's clipboard only (not to other
    /// devices through Universal Clipboard). A secret (a password, a hidden
    /// field) leaves the clipboard after 90 seconds.
    func copy(_ text: String, what: String, secret: Bool) {
        var options: [UIPasteboard.OptionsKey: Any] = [.localOnly: true]
        if secret {
            options[.expirationDate] = Date().addingTimeInterval(Self.clipboardLifetime)
        }
        UIPasteboard.general.setItems([[UTType.plainText.identifier: text]], options: options)
        core.touch()
        NotesStore.shared.show(secret ? "Copied \(what). It's cleared from the clipboard in 90 seconds." : "Copied \(what).")
    }
}

/// Hears from the vault's background threads, and has the store redraw on
/// the main thread.
final class VaultListener: PasswordsListener, @unchecked Sendable {
    weak var store: PasswordsStore?

    func vaultChanged() {
        DispatchQueue.main.async { [weak self] in
            self?.store?.changed()
        }
    }
}
