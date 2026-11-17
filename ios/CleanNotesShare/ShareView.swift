import SwiftUI
import UIKit

/// The share extension's principal class: the SwiftUI sheet in it.
@objc(ShareViewController)
final class ShareViewController: UIViewController {
    private var model: ShareModel?

    override func viewDidLoad() {
        super.viewDidLoad()
        let model = ShareModel(context: extensionContext)
        self.model = model
        let host = UIHostingController(rootView: ShareView(model: model))
        addChild(host)
        host.view.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(host.view)
        NSLayoutConstraint.activate([
            host.view.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            host.view.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            host.view.topAnchor.constraint(equalTo: view.topAnchor),
            host.view.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
        host.didMove(toParent: self)
        model.load()
    }
}

/// Apple Notes' share sheet: what to add (typed text over the link, pictures
/// and files shared), and which note it goes to, a new one (in a folder) or
/// one already there.
struct ShareView: View {
    @ObservedObject var model: ShareModel
    @FocusState private var typing: Bool

    var body: some View {
        NavigationStack {
            Form {
                if let problem = model.problem {
                    Section {
                        Label(problem, systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.secondary)
                    }
                }
                Section {
                    ZStack(alignment: .topLeading) {
                        if model.text.isEmpty {
                            Text("Add text to your note")
                                .foregroundStyle(.tertiary)
                                .padding(.top, 8)
                                .padding(.leading, 5)
                                .allowsHitTesting(false)
                        }
                        TextEditor(text: $model.text)
                            .focused($typing)
                            .frame(minHeight: 110)
                            .scrollContentBackground(.hidden)
                    }
                    ForEach(model.links) { link in
                        LinkCard(link: link)
                    }
                    if !pictures.isEmpty {
                        ScrollView(.horizontal, showsIndicators: false) {
                            HStack(spacing: 8) {
                                ForEach(pictures) { f in
                                    Thumbnail(file: f)
                                        .contextMenu {
                                            Button("Remove", systemImage: "trash", role: .destructive) { model.remove(f) }
                                        }
                                }
                            }
                            .padding(.vertical, 4)
                        }
                    }
                    ForEach(others) { f in
                        Label(f.name, systemImage: "doc")
                            .lineLimit(1)
                            .swipeActions {
                                Button("Remove", role: .destructive) { model.remove(f) }
                            }
                    }
                    if model.loading > 0 && model.links.isEmpty && model.files.isEmpty {
                        HStack {
                            Spacer()
                            ProgressView()
                            Spacer()
                        }
                    }
                }
                Section {
                    NavigationLink {
                        NotePicker(model: model)
                    } label: {
                        LabeledContent("Choose Note", value: model.noteTitle)
                    }
                    if model.noteId == nil && !model.catalog.folders.isEmpty {
                        NavigationLink {
                            FolderPicker(model: model)
                        } label: {
                            LabeledContent("Folder", value: model.folderName)
                        }
                    }
                }
            }
            .navigationTitle("Notes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { model.cancel() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") { model.save() }
                        .bold()
                        .disabled(!model.canSave)
                }
            }
        }
        .tint(Color(uiColor: .shareAccent))
        .onAppear { typing = true }
    }

    private var pictures: [ShareModel.File] { model.files.filter(\.isImage) }
    private var others: [ShareModel.File] { model.files.filter { !$0.isImage } }
}

/// A shared page: its picture, title and site.
private struct LinkCard: View {
    let link: ShareModel.Link

    var body: some View {
        HStack(spacing: 12) {
            Group {
                if let image = link.image {
                    Image(uiImage: image).resizable().scaledToFill()
                } else {
                    Image(systemName: "safari").font(.title2).foregroundStyle(.secondary)
                }
            }
            .frame(width: 56, height: 56)
            .background(Color(.tertiarySystemFill))
            .clipShape(RoundedRectangle(cornerRadius: 8))
            VStack(alignment: .leading, spacing: 2) {
                Text(link.title ?? link.url.absoluteString)
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(2)
                Text(link.url.host() ?? link.url.absoluteString)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
    }
}

private struct Thumbnail: View {
    let file: ShareModel.File

    var body: some View {
        Group {
            if let t = file.thumbnail {
                Image(uiImage: t).resizable().scaledToFill()
            } else {
                Image(systemName: "photo").foregroundStyle(.secondary)
            }
        }
        .frame(width: 72, height: 72)
        .background(Color(.tertiarySystemFill))
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }
}

/// "Choose Note": a new note, or one of the notes (most recent first), with
/// search.
private struct NotePicker: View {
    @ObservedObject var model: ShareModel
    @Environment(\.dismiss) private var dismiss
    @State private var search = ""

    var body: some View {
        List {
            if search.isEmpty {
                row(id: nil) {
                    Label("New Note", systemImage: "square.and.pencil")
                }
            }
            Section {
                ForEach(found) { n in
                    row(id: n.id) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(n.title.isEmpty ? "New Note" : n.title)
                                .font(.body.weight(.semibold))
                                .lineLimit(1)
                            HStack(spacing: 6) {
                                Text(n.time)
                                if !n.folder.isEmpty {
                                    Label(n.folder, systemImage: "folder")
                                        .labelStyle(.titleAndIcon)
                                }
                            }
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                        }
                    }
                }
            } header: {
                if !model.catalog.notes.isEmpty && search.isEmpty { Text("Notes") }
            } footer: {
                if model.catalog.notes.isEmpty {
                    Text("Open CleanNotes once to choose from its notes here.")
                }
            }
        }
        .searchable(text: $search, placement: .navigationBarDrawer(displayMode: .always))
        .navigationTitle("Choose Note")
        .navigationBarTitleDisplayMode(.inline)
    }

    private var found: [ShareCatalog.Note] {
        let words = search.lowercased().split(separator: " ")
        guard !words.isEmpty else { return model.catalog.notes }
        return model.catalog.notes.filter { n in
            let t = n.title.lowercased()
            return words.allSatisfy { t.contains($0) }
        }
    }

    private func row<L: View>(id: String?, @ViewBuilder label: () -> L) -> some View {
        Button {
            model.noteId = id
            dismiss()
        } label: {
            HStack {
                label()
                Spacer()
                if model.noteId == id {
                    Image(systemName: "checkmark").foregroundStyle(.tint).fontWeight(.semibold)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// The folder a new note goes to, account by account.
private struct FolderPicker: View {
    @ObservedObject var model: ShareModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        List {
            ForEach(accounts, id: \.self) { account in
                Section(accounts.count > 1 ? account : "") {
                    ForEach(model.catalog.folders.filter { $0.account == account }) { f in
                        Button {
                            model.folderId = f.id
                            dismiss()
                        } label: {
                            HStack {
                                Label(f.name, systemImage: "folder")
                                    .padding(.leading, CGFloat(f.depth) * 20)
                                Spacer()
                                if model.folderId == f.id {
                                    Image(systemName: "checkmark").foregroundStyle(.tint).fontWeight(.semibold)
                                }
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
        }
        .navigationTitle("Folder")
        .navigationBarTitleDisplayMode(.inline)
    }

    /// The accounts, in order.
    private var accounts: [String] {
        var seen: [String] = []
        for f in model.catalog.folders where !seen.contains(f.account) {
            seen.append(f.account)
        }
        return seen
    }
}

extension UIColor {
    /// The app's accent (its AccentColor), in light and dark.
    static let shareAccent = UIColor { t in
        t.userInterfaceStyle == .dark
            ? UIColor(red: 1.0, green: 0.839, blue: 0.039, alpha: 1)
            : UIColor(red: 0.886, green: 0.651, blue: 0.0, alpha: 1)
    }
}
