import CoreSpotlight
import SwiftUI
import UIKit

/// The Home Screen quick action, as in Apple Notes: touch and hold the
/// app's icon for New Note.
enum QuickAction {
    static let newNote = "com.cleannotes.new-note"

    static var items: [UIApplicationShortcutItem] {
        [
            UIApplicationShortcutItem(
                type: newNote,
                localizedTitle: "New Note",
                localizedSubtitle: nil,
                icon: UIApplicationShortcutIcon(systemImageName: "square.and.pencil"),
                userInfo: nil
            ),
        ]
    }
}

/// Where something outside the app asks it to go: the quick action, a note
/// found with Spotlight, Shortcuts and Siri.
enum AppRequest: Equatable {
    /// A new note, open with the keyboard up, in a folder (or where new notes
    /// go).
    case newNote(folder: String?)
    /// A note, over its folder's list; `editing` puts the keyboard up.
    case openNote(id: String, editing: Bool)
    /// A notes list.
    case openPlace(Place)

    /// A quick action's request.
    init?(shortcut type: String) {
        guard type == QuickAction.newNote else { return nil }
        self = .newNote(folder: nil)
    }

    /// A note tapped in Spotlight's results.
    init?(activity: NSUserActivity) {
        guard activity.activityType == CSSearchableItemActionType,
              let id = activity.userInfo?[CSSearchableItemActivityIdentifier] as? String
        else { return nil }
        self = .openNote(id: id, editing: false)
    }
}

/// The request waiting for the screens to act on it.
final class AppRequests: ObservableObject {
    static let shared = AppRequests()
    @Published var pending: AppRequest?
}

/// Hears of quick actions and Spotlight results: one that launched the app
/// comes with its scene, one chosen while it runs goes to the scene's
/// delegate.
final class AppDelegate: NSObject, UIApplicationDelegate {
    func application(
        _ application: UIApplication,
        configurationForConnecting session: UISceneSession,
        options: UIScene.ConnectionOptions
    ) -> UISceneConfiguration {
        if let item = options.shortcutItem, let request = AppRequest(shortcut: item.type) {
            AppRequests.shared.pending = request
        }
        if let request = options.userActivities.lazy.compactMap(AppRequest.init(activity:)).first {
            AppRequests.shared.pending = request
        }
        let config = UISceneConfiguration(name: nil, sessionRole: session.role)
        config.delegateClass = SceneDelegate.self
        return config
    }
}

final class SceneDelegate: NSObject, UIWindowSceneDelegate {
    func windowScene(
        _ windowScene: UIWindowScene,
        performActionFor shortcutItem: UIApplicationShortcutItem,
        completionHandler: @escaping (Bool) -> Void
    ) {
        let request = AppRequest(shortcut: shortcutItem.type)
        AppRequests.shared.pending = request
        completionHandler(request != nil)
    }

    func scene(_ scene: UIScene, continue userActivity: NSUserActivity) {
        if let request = AppRequest(activity: userActivity) {
            AppRequests.shared.pending = request
        }
    }
}
