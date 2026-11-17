import CoreSpotlight
import XCTest
@testable import CleanNotes

/// The notes given to the system's search: only what changed is sent again,
/// notes that go are taken away, and a locked note gives its title only.
/// (An index of its own, not the app's.)
final class SpotlightTests: XCTestCase {
    private var dir: URL!
    private var store: NotesStore!
    private var spotlight: SpotlightIndex!

    override func setUpWithError() throws {
        dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        store = NotesStore(directory: dir.appendingPathComponent("notes", isDirectory: true))
        XCTAssertNil(store.startupError)
        spotlight = SpotlightIndex(
            index: CSSearchableIndex(name: "tests-\(UUID().uuidString)"),
            stateFile: dir.appendingPathComponent("spotlight.json")
        )
    }

    override func tearDownWithError() throws {
        store = nil
        spotlight = nil
        try? FileManager.default.removeItem(at: dir)
    }

    func testOnlyChangesAreSent() throws {
        let trip = store.core.createNote(folder: nil, text: "Trip\nPack the bags #travel")
        let shop = store.core.createNote(folder: nil, text: "Shopping\nEggs")
        var sent = spotlight.updateNow(store.core)
        XCTAssertEqual(Set(sent.changed), [trip.id, shop.id])
        XCTAssertEqual(sent.removed, [])

        sent = spotlight.updateNow(store.core)
        XCTAssertEqual(sent.changed, [], "nothing changed: nothing sent")

        try store.core.addText(id: trip.id, text: "Passport")
        store.core.trashNotes(ids: [shop.id], place: shop.place)
        sent = spotlight.updateNow(store.core)
        XCTAssertEqual(sent.changed, [trip.id])
        XCTAssertEqual(sent.removed, [shop.id], "Recently Deleted isn't searched")

        // Another start remembers what the system has.
        let again = SpotlightIndex(index: CSSearchableIndex(name: "tests-\(UUID().uuidString)"), stateFile: dir.appendingPathComponent("spotlight.json"))
        XCTAssertEqual(again.updateNow(store.core).changed, [])
    }

    func testEntriesSayWhatTheNoteSays() throws {
        let n = store.core.createNote(folder: nil, text: "Trip\nPack the bags #travel\nBook a taxi")
        let indexed = try XCTUnwrap(store.core.indexedNotes().first { $0.id == n.id })
        let a = SpotlightIndex.item(indexed).attributeSet
        XCTAssertEqual(a.title, "Trip")
        XCTAssertEqual(a.contentDescription, "Pack the bags #travel Book a taxi")
        XCTAssertEqual(a.textContent, "Pack the bags #travel\nBook a taxi")
        XCTAssertEqual(a.keywords, ["travel", "Notes"])
    }

    func testLockedNotesGiveTheirTitleOnly() throws {
        let n = store.core.createNote(folder: nil, text: "Bank\nPIN 1234")
        try store.core.setPassword(password: "pw", hint: "")
        XCTAssertEqual(store.core.lockNote(id: n.id), .locked)
        let indexed = try XCTUnwrap(store.core.indexedNotes().first { $0.id == n.id })
        let a = SpotlightIndex.item(indexed).attributeSet
        XCTAssertEqual(a.title, "Bank")
        XCTAssertEqual(a.contentDescription, "Locked")
        XCTAssertNil(a.textContent)
        XCTAssertThrowsError(try store.core.addText(id: n.id, text: "x"), "Shortcuts can't change a locked note")
    }

    func testTappedResultsOpenTheirNote() {
        let tapped = NSUserActivity(activityType: CSSearchableItemActionType)
        tapped.userInfo = [CSSearchableItemActivityIdentifier: "note-1"]
        XCTAssertEqual(AppRequest(activity: tapped), .openNote(id: "note-1", editing: false))
        XCTAssertNil(AppRequest(activity: NSUserActivity(activityType: "something.else")))
        XCTAssertEqual(AppRequest(shortcut: QuickAction.newNote), .newNote(folder: nil))
    }
}
