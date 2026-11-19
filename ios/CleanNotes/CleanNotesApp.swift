import CoreSpotlight
import SwiftUI
import UIKit

@main
struct CleanNotesApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var store = NotesStore.shared
    @Environment(\.scenePhase) private var phase

    var body: some Scene {
        WindowGroup {
            Group {
                if let error = store.startupError {
                    StartupErrorView(error: error)
                } else {
                    RootView()
                }
            }
            .environmentObject(store)
            .tint(.notesAccent)
        }
        .onChange(of: phase) { _, phase in
            switch phase {
            case .active:
                store.becameActive()
                PasswordsStore.shared.becameActive()
                UIApplication.shared.shortcutItems = store.startupError == nil ? QuickAction.items : []
            case .background, .inactive:
                store.resignedActive()
                PasswordsStore.shared.resignedActive()
            @unknown default: break
            }
        }
    }
}

/// The navigation stack, starting where the app was left: Folders, then the
/// last folder's notes (as Apple Notes does).
struct RootView: View {
    @EnvironmentObject private var store: NotesStore
    @State private var path: [Route] = []
    @State private var restored = false
    @ObservedObject private var requests = AppRequests.shared

    var body: some View {
        NavigationStack(path: $path) {
            FoldersView(path: $path)
                .navigationDestination(for: Route.self) { route in
                    switch route {
                    case let .list(place):
                        NotesListView(place: place, path: $path)
                    case let .note(id, isNew):
                        NoteView(noteId: id, startEditing: isNew, path: $path)
                    case let .passwords(place):
                        PasswordListView(place: place)
                    case let .password(id):
                        PasswordDetailView(id: id)
                    }
                }
        }
        .overlay(alignment: .bottom) { NoticeView() }
        .onAppear {
            guard !restored else { return }
            restored = true
            path = [.list(store.core.initialPlace())]
            // Launched from the quick action, Spotlight or Shortcuts.
            handleRequest()
        }
        .onChange(of: requests.pending) { _, _ in
            if restored { handleRequest() }
        }
        .onContinueUserActivity(CSSearchableItemActionType) { activity in
            if let request = AppRequest(activity: activity) {
                requests.pending = request
            }
        }
    }

    /// Goes where the quick action, Spotlight or Shortcuts asked.
    private func handleRequest() {
        guard let request = requests.pending else { return }
        requests.pending = nil
        store.saveEditor()
        switch request {
        case let .newNote(folder):
            // As New Note from the Home Screen: open with the keyboard up,
            // over that folder's list.
            let n = store.core.newNote(place: .folder(id: folder ?? store.core.defaultFolder()))
            path = [.list(n.place), .note(id: n.id, isNew: true)]
        case let .openNote(id, editing):
            guard let place = store.core.notePlace(id: id) else {
                store.show("The note can't be found.")
                return
            }
            path = [.list(place), .note(id: id, isNew: editing)]
        case let .openPlace(place):
            guard store.core.placeExists(place: place) else {
                store.show("The folder can't be found.")
                return
            }
            path = [.list(place)]
        }
    }
}

/// Messages from the core ("Note recovered.", sync conflicts, …).
struct NoticeView: View {
    @EnvironmentObject private var store: NotesStore

    var body: some View {
        // The animation sits outside the `if`, so the notice slides in and out.
        ZStack {
            if let text = store.notice {
                Text(text)
                    .font(.footnote)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
                    .background(.regularMaterial, in: Capsule())
                    .padding(.horizontal, 24)
                    .padding(.bottom, 64)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                    .onTapGesture { store.notice = nil }
            }
        }
        .animation(.spring, value: store.notice)
    }
}

struct StartupErrorView: View {
    let error: String

    var body: some View {
        ContentUnavailableView {
            Label("Your notes can't be opened", systemImage: "exclamationmark.triangle")
        } description: {
            Text(error)
        }
    }
}
