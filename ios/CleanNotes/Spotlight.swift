import CoreSpotlight
import CryptoKit
import Foundation
import UniformTypeIdentifiers

/// The notes in the iPhone's search (Spotlight), as Apple Notes' are: each
/// note's title, text, folder and #tags, so searching from the Home Screen
/// finds it and tapping it opens it here (`AppRequests`). A locked note is
/// found by its title only; its text never leaves the app.
///
/// What was last given to the system is remembered (a digest of each note's
/// entry, in Caches), so an update only sends the notes that
/// changed and takes away the ones that are gone.
final class SpotlightIndex {
    static let shared = SpotlightIndex()

    /// The identifier every note's entry is filed under.
    static let domain = "notes"

    private let queue = DispatchQueue(label: "com.cleannotes.spotlight", qos: .utility)
    private let index: CSSearchableIndex
    private let stateFile: URL
    /// What the system has, by note id. Only touched on `queue`.
    private var indexed: [String: String]?
    private var pending: DispatchWorkItem?

    init(index: CSSearchableIndex = .default(), stateFile: URL? = nil) {
        self.index = index
        self.stateFile = stateFile ?? FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("spotlight-index.json")
    }

    /// Brings the system's search up to date a moment from now (changes come
    /// in bursts: a sync, several notes moved), then runs `then` if notes
    /// were added, changed or taken away.
    func update(_ core: NotesCore, after delay: TimeInterval = 1, then: (() -> Void)? = nil) {
        pending?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self else { return }
            let (changed, removed) = self.updateNow(core)
            if !changed.isEmpty || !removed.isEmpty { then?() }
        }
        pending = work
        queue.asyncAfter(deadline: .now() + delay, execute: work)
    }

    /// Brings the system's search up to date (on the caller's thread), and
    /// says what it sent: the notes added or changed, and those taken away.
    @discardableResult
    func updateNow(_ core: NotesCore) -> (changed: [String], removed: [String]) {
        let notes = core.indexedNotes()
        let old = indexed ?? loadState()
        var now: [String: String] = [:]
        var items: [CSSearchableItem] = []
        for n in notes {
            let stamp = Self.fingerprint(n)
            now[n.id] = stamp
            if old[n.id] != stamp {
                items.append(Self.item(n))
            }
        }
        let removed = old.keys.filter { now[$0] == nil }
        // Remembered once the system has them: if it fails, the next update
        // sends them again.
        let done = DispatchGroup()
        var failed = false
        if !items.isEmpty {
            done.enter()
            index.indexSearchableItems(items) { error in
                if error != nil { failed = true }
                done.leave()
            }
        }
        if !removed.isEmpty {
            done.enter()
            index.deleteSearchableItems(withIdentifiers: removed) { error in
                if error != nil { failed = true }
                done.leave()
            }
        }
        done.wait()
        if !failed {
            indexed = now
            saveState(now)
        } else {
            indexed = nil
        }
        return (items.map(\.uniqueIdentifier), removed)
    }

    /// Takes every note out of the system's search (and forgets it was there).
    func removeAll() {
        queue.async { [self] in
            index.deleteSearchableItems(withDomainIdentifiers: [Self.domain]) { _ in }
            indexed = [:]
            saveState([:])
        }
    }

    // ---- Entries ----

    /// A digest of everything the note's entry says: when it changes, the
    /// entry is sent again.
    static func fingerprint(_ n: IndexedNote) -> String {
        let all = [String(n.modified), String(n.locked), n.folder, n.title, n.tags.joined(separator: " "), n.text]
            .joined(separator: "\u{0}")
        return SHA256.hash(data: Data(all.utf8)).map { String(format: "%02x", $0) }.joined()
    }

    static func item(_ n: IndexedNote) -> CSSearchableItem {
        let a = CSSearchableItemAttributeSet(contentType: .text)
        a.title = n.title
        a.displayName = n.title
        if n.locked {
            a.contentDescription = "Locked"
        } else {
            // The lines after the title, as the notes list shows them; the
            // whole text is searched.
            a.contentDescription = String(n.text.split(separator: "\n").joined(separator: " ").prefix(300))
            a.textContent = n.text
        }
        a.keywords = n.tags + (n.folder.isEmpty ? [] : [n.folder])
        let date = Date(timeIntervalSince1970: TimeInterval(n.modified))
        a.contentModificationDate = date
        a.lastUsedDate = date
        return CSSearchableItem(uniqueIdentifier: n.id, domainIdentifier: domain, attributeSet: a)
    }

    // ---- What the system has ----

    private func loadState() -> [String: String] {
        guard let data = try? Data(contentsOf: stateFile),
              let state = try? JSONDecoder().decode([String: String].self, from: data)
        else { return [:] }
        return state
    }

    private func saveState(_ state: [String: String]) {
        if let data = try? JSONEncoder().encode(state) {
            try? data.write(to: stateFile, options: .atomic)
        }
    }
}
