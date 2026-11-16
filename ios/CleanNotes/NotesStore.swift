import Foundation
import SwiftUI

/// The app's model: the Rust core (the same notes library the desktop app
/// runs on) and what the screens need to know when it changes.
///
/// Screens read straight from `core` while they draw (it's quick); `revision`
/// goes up whenever something changed, which redraws them.
final class NotesStore: ObservableObject {
    let core: NotesCore
    /// Goes up when notes, folders, accounts or settings change.
    @Published private(set) var revision = 0
    /// A message shown at the bottom for a few seconds.
    @Published var notice: String?
    /// The core couldn't open the notes.
    let startupError: String?

    /// The editor of the note on screen: saved before anything syncs.
    weak var activeEditor: NoteEditor?
    /// Told when a sync changed notes (the open note reloads).
    var onSyncChange: [UUID: () -> Void] = [:]

    private var timer: Timer?
    private var noticeTask: Task<Void, Never>?

    init() {
        let listener = SyncListener()
        var failure: String?
        var opened: NotesCore?
        do {
            let dir = NotesStore.dataDirectory()
            let seed = ProcessInfo.processInfo.environment["CLEANNOTES_SEED"] != nil
            opened = try NotesCore(dir: dir.path, listener: listener, seed: seed)
        } catch {
            failure = message(error)
        }
        // Without its notes the app can only say why.
        self.core = opened ?? NotesStore.fallbackCore(listener: listener)
        self.startupError = opened == nil ? failure : nil
        listener.store = self
        let core = self.core
        NoteImages.shared.path = { name in core.attachmentPath(name: name) }
    }

    /// Application Support/CleanNotes.
    static func dataDirectory() -> URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let dir = base.appendingPathComponent("CleanNotes", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    /// An empty library in a temporary folder, for the error screen.
    private static func fallbackCore(listener: SyncListener) -> NotesCore {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        // swiftlint:disable:next force_try
        return try! NotesCore(dir: dir.path, listener: listener, seed: false)
    }

    // ---- Over time ----

    /// The app came to the foreground.
    func becameActive() {
        tick()
        core.syncAll()
        timer?.invalidate()
        // Fires on the main run loop.
        timer = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { [weak self] _ in
            self?.tick()
        }
    }

    /// The app went to the background.
    func resignedActive() {
        saveEditor()
        core.saveNow()
        timer?.invalidate()
        timer = nil
    }

    /// Sync replies, syncs that are due, saving, auto-lock.
    func tick() {
        saveEditor()
        let t = core.tick()
        if t.changed || t.autoLocked || t.attachments {
            // (Pictures arriving: the open note shows them.)
            changed()
            for f in onSyncChange.values { f() }
        }
        if let n = t.notices.last {
            show(n)
        }
    }

    func saveEditor() {
        if let e = activeEditor {
            _ = core.saveEditor(editor: e)
        }
    }

    /// Something changed: screens redraw.
    func changed() {
        revision &+= 1
        if let n = core.takeNotices().last {
            show(n)
        }
    }

    func show(_ text: String) {
        notice = text
        noticeTask?.cancel()
        noticeTask = Task { @MainActor in
            try? await Task.sleep(for: .seconds(4))
            if !Task.isCancelled { self.notice = nil }
        }
    }

    /// Runs `f` and redraws.
    func perform(_ f: (NotesCore) -> Void) {
        f(core)
        changed()
    }

    /// Pull to refresh: sync everything, and wait (a while) for it. (On the
    /// main actor: an async method would otherwise run elsewhere.)
    @MainActor
    func refresh() async {
        saveEditor()
        core.syncAll()
        for _ in 0..<40 {
            try? await Task.sleep(for: .milliseconds(250))
            tick()
            if !core.anySyncing() { break }
        }
    }
}

/// Hears from the sync thread, and has the store look on the main thread.
final class SyncListener: CoreListener, @unchecked Sendable {
    weak var store: NotesStore?

    func syncReplied() {
        DispatchQueue.main.async { [weak self] in
            self?.store?.tick()
        }
    }
}
