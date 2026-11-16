import SwiftUI
import UIKit

@main
struct CleanNotesApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var store = NotesStore()
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
                UIApplication.shared.shortcutItems = store.startupError == nil ? QuickAction.items : []
            case .background, .inactive: store.resignedActive()
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
    @ObservedObject private var quickActions = QuickActionCenter.shared

    var body: some View {
        NavigationStack(path: $path) {
            FoldersView(path: $path)
                .navigationDestination(for: Route.self) { route in
                    switch route {
                    case let .list(place):
                        NotesListView(place: place, path: $path)
                    case let .note(id, isNew):
                        NoteView(noteId: id, startEditing: isNew, path: $path)
                    }
                }
        }
        .overlay(alignment: .bottom) { NoticeView() }
        .onAppear {
            guard !restored else { return }
            restored = true
            path = [.list(store.core.initialPlace())]
            // Launched from the quick action.
            handleQuickAction()
        }
        .onChange(of: quickActions.pending) { _, _ in
            if restored { handleQuickAction() }
        }
    }

    /// New Note from the Home Screen: a note in the default folder, open with
    /// the keyboard up, over that folder's list.
    private func handleQuickAction() {
        guard let action = quickActions.pending else { return }
        quickActions.pending = nil
        guard action == QuickAction.newNote else { return }
        store.saveEditor()
        let n = store.core.newNote(place: .folder(id: store.core.defaultFolder()))
        path = [.list(n.place), .note(id: n.id, isNew: true)]
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
