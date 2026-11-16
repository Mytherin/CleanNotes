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

/// The quick action chosen, for the screens to act on.
final class QuickActionCenter: ObservableObject {
    static let shared = QuickActionCenter()
    @Published var pending: String?
}

/// Hears of quick actions: one that launched the app comes with its scene,
/// one chosen while it runs goes to the scene's delegate.
final class AppDelegate: NSObject, UIApplicationDelegate {
    func application(
        _ application: UIApplication,
        configurationForConnecting session: UISceneSession,
        options: UIScene.ConnectionOptions
    ) -> UISceneConfiguration {
        if let item = options.shortcutItem {
            QuickActionCenter.shared.pending = item.type
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
        QuickActionCenter.shared.pending = shortcutItem.type
        completionHandler(true)
    }
}
