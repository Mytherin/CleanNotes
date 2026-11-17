import Foundation
import ImageIO
import LinkPresentation
import UIKit
import UniformTypeIdentifiers

/// What's being shared, and where it's going: read from the share sheet's
/// items, and left in the App Group's inbox on Save for the app to add.
@MainActor
final class ShareModel: ObservableObject {
    struct Link: Identifiable {
        let id = UUID()
        let url: URL
        var title: String?
        var image: UIImage?
    }

    struct File: Identifiable {
        let id = UUID()
        /// Its copy in the draft folder.
        let url: URL
        let name: String
        let isImage: Bool
        var thumbnail: UIImage?
    }

    @Published var text = ""
    @Published var links: [Link] = []
    @Published var files: [File] = []
    /// Items still being read.
    @Published private(set) var loading = 0
    /// The note to add to; nil for a new note.
    @Published var noteId: String?
    @Published var folderId: String?
    /// Why it can't be saved.
    @Published private(set) var problem: String?

    let catalog: ShareCatalog
    private let inbox: ShareInbox?
    /// Where the shared files are copied until Save (none without the
    /// App Group).
    nonisolated private let draftFolder: URL?
    /// Saved or cancelled: the draft is gone.
    private var finished = false
    private weak var context: NSExtensionContext?

    init(context: NSExtensionContext?) {
        self.context = context
        inbox = ShareInbox.shared
        catalog = inbox?.readCatalog() ?? ShareCatalog(notes: [], folders: [], defaultFolder: "")
        let known = Set(catalog.folders.map(\.id))
        if let last = ShareMemory.lastFolder, known.contains(last) {
            folderId = last
        } else if known.contains(catalog.defaultFolder) {
            folderId = catalog.defaultFolder
        }
        draftFolder = try? inbox?.newDraft()
        if inbox == nil {
            problem = "This copy of CleanNotes was signed without its App Group, so notes can't be added from other apps."
        } else if draftFolder == nil {
            problem = "The shared items can't be kept for the app."
        }
    }

    // ---- Where it goes ----

    var noteTitle: String {
        guard let id = noteId else { return "New Note" }
        let t = catalog.notes.first { $0.id == id }?.title ?? ""
        return t.isEmpty ? "New Note" : t
    }

    var folderName: String {
        catalog.folders.first { $0.id == folderId }?.name ?? "Notes"
    }

    var canSave: Bool {
        problem == nil && loading == 0 && !finished
            && !(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && links.isEmpty && files.isEmpty)
    }

    // ---- Reading what's shared ----

    func load() {
        let items = (context?.inputItems as? [NSExtensionItem]) ?? []
        for item in items {
            for provider in item.attachments ?? [] {
                read(provider)
            }
        }
    }

    private func read(_ p: NSItemProvider) {
        let fileURL = UTType.fileURL.identifier
        if p.hasItemConformingToTypeIdentifier(UTType.url.identifier) && !p.hasItemConformingToTypeIdentifier(fileURL)
            && !p.hasItemConformingToTypeIdentifier(UTType.image.identifier) {
            begin()
            p.loadItem(forTypeIdentifier: UTType.url.identifier) { [weak self] item, _ in
                let url = (item as? URL) ?? (item as? NSURL as URL?) ?? (item as? Data).flatMap { URL(dataRepresentation: $0, relativeTo: nil) }
                Task { @MainActor in
                    if let url {
                        if url.isFileURL { self?.copyFile(url, name: url.lastPathComponent) } else { self?.addLink(url) }
                    }
                    self?.end()
                }
            }
        } else if p.hasItemConformingToTypeIdentifier(UTType.image.identifier) {
            readImage(p)
        } else if p.hasItemConformingToTypeIdentifier(UTType.plainText.identifier) && !p.hasItemConformingToTypeIdentifier(fileURL) {
            begin()
            p.loadItem(forTypeIdentifier: UTType.plainText.identifier) { [weak self] item, _ in
                let s = (item as? String) ?? (item as? NSAttributedString)?.string ?? (item as? Data).flatMap { String(data: $0, encoding: .utf8) }
                Task { @MainActor in
                    if let s, let self {
                        self.text = self.text.isEmpty ? s : self.text + "\n" + s
                    }
                    self?.end()
                }
            }
        } else {
            readFile(p, type: p.registeredTypeIdentifiers.first { UTType($0)?.conforms(to: .item) == true } ?? UTType.data.identifier)
        }
    }

    /// A picture: its file if there is one (Photos), else its pixels (a
    /// screenshot).
    private func readImage(_ p: NSItemProvider) {
        let type = p.registeredTypeIdentifiers.first { UTType($0)?.conforms(to: .image) == true } ?? UTType.image.identifier
        let suggested = p.suggestedName
        // Asked again below, for the pixels, if there's no file.
        nonisolated(unsafe) let provider = p
        begin()
        p.loadFileRepresentation(forTypeIdentifier: type) { [weak self] url, _ in
            // The file is gone when this returns: copy it now.
            let name = suggested.map { Self.named($0, type: type) } ?? url?.lastPathComponent ?? "Picture.jpg"
            let copied = url.flatMap { self?.copyNow($0, name: name) }
            if let copied {
                Task { @MainActor in
                    self?.addFile(copied, name: name, isImage: true)
                    self?.end()
                }
                return
            }
            provider.loadItem(forTypeIdentifier: UTType.image.identifier) { [weak self] item, _ in
                var data: Data?
                if let img = item as? UIImage {
                    data = img.pngData()
                } else if let d = item as? Data {
                    data = d
                } else if let u = item as? URL {
                    data = try? Data(contentsOf: u)
                }
                let written = data.flatMap { self?.writeNow($0, name: "Picture.png") }
                Task { @MainActor in
                    if let written { self?.addFile(written, name: written.lastPathComponent, isImage: true) }
                    self?.end()
                }
            }
        }
    }

    private func readFile(_ p: NSItemProvider, type: String) {
        let suggested = p.suggestedName
        begin()
        p.loadFileRepresentation(forTypeIdentifier: type) { [weak self] url, _ in
            let name = suggested.map { Self.named($0, type: type) } ?? url?.lastPathComponent ?? "File"
            let copied = url.flatMap { self?.copyNow($0, name: name) }
            Task { @MainActor in
                if let copied {
                    let isImage = UTType(filenameExtension: copied.pathExtension)?.conforms(to: .image) == true
                    self?.addFile(copied, name: name, isImage: isImage)
                }
                self?.end()
            }
        }
    }

    /// A suggested name with the type's extension, if it has none.
    nonisolated private static func named(_ name: String, type: String) -> String {
        guard (name as NSString).pathExtension.isEmpty, let ext = UTType(type)?.preferredFilenameExtension else { return name }
        return name + "." + ext
    }

    private func copyFile(_ url: URL, name: String) {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        if let copied = copyNow(url, name: name) {
            let isImage = UTType(filenameExtension: url.pathExtension)?.conforms(to: .image) == true
            addFile(copied, name: name, isImage: isImage)
        }
    }

    /// Copies into the draft (from any thread).
    nonisolated private func copyNow(_ url: URL, name: String) -> URL? {
        guard let dest = slot(for: name) else { return nil }
        do {
            try FileManager.default.copyItem(at: url, to: dest)
            return dest
        } catch {
            return nil
        }
    }

    nonisolated private func writeNow(_ data: Data, name: String) -> URL? {
        guard let dest = slot(for: name) else { return nil }
        return (try? data.write(to: dest)) == nil ? nil : dest
    }

    /// A new place in the draft for a file (a folder each, so names may
    /// repeat).
    nonisolated private func slot(for name: String) -> URL? {
        guard let draft = draftFolder else { return nil }
        let dir = draft.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let safe = name.replacingOccurrences(of: "/", with: "-")
        return dir.appendingPathComponent(safe.isEmpty ? "File" : safe)
    }

    private func addFile(_ url: URL, name: String, isImage: Bool) {
        let file = File(url: url, name: name, isImage: isImage, thumbnail: isImage ? Self.thumbnail(url) : nil)
        files.append(file)
    }

    private func addLink(_ url: URL) {
        let link = Link(url: url)
        links.append(link)
        // The page's title and picture, as Apple Notes shows them.
        let provider = LPMetadataProvider()
        provider.timeout = 8
        begin()
        provider.startFetchingMetadata(for: url) { [weak self] meta, _ in
            let title = meta?.title
            let imageProvider = meta?.imageProvider ?? meta?.iconProvider
            Task { @MainActor in
                if let i = self?.links.firstIndex(where: { $0.id == link.id }) {
                    self?.links[i].title = title
                }
                self?.end()
            }
            imageProvider?.loadObject(ofClass: UIImage.self) { [weak self] image, _ in
                let image = image as? UIImage
                Task { @MainActor in
                    if let i = self?.links.firstIndex(where: { $0.id == link.id }) {
                        self?.links[i].image = image
                    }
                }
            }
        }
    }

    /// A small picture to show, without reading the whole image.
    private static func thumbnail(_ url: URL) -> UIImage? {
        guard let src = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        let opts: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 240,
        ]
        return CGImageSourceCreateThumbnailAtIndex(src, 0, opts as CFDictionary).map { UIImage(cgImage: $0) }
    }

    private func begin() { loading += 1 }
    private func end() { loading = max(0, loading - 1) }

    func remove(_ file: File) {
        files.removeAll { $0.id == file.id }
        try? FileManager.default.removeItem(at: file.url.deletingLastPathComponent())
    }

    // ---- Done ----

    func save() {
        guard canSave, let inbox, let draft = draftFolder else { return }
        let typed = text
        var drop = SharedDrop(noteId: noteId, folderId: noteId == nil ? folderId : nil, text: typed)
        // A link already in the text isn't added again.
        drop.links = links
            .filter { !typed.contains($0.url.absoluteString) }
            .map { SharedDrop.Link(url: $0.url.absoluteString, title: $0.title) }
        let base = draft.standardizedFileURL.path
        drop.files = files.map { f in
            var rel = f.url.standardizedFileURL.path
            if rel.hasPrefix(base + "/") { rel.removeFirst(base.count + 1) }
            return SharedDrop.File(path: rel, name: f.name, isImage: f.isImage)
        }
        do {
            try inbox.post(drop, draft: draft)
            finished = true
            if noteId == nil, let folderId { ShareMemory.lastFolder = folderId }
            context?.completeRequest(returningItems: nil)
        } catch {
            problem = "This couldn't be saved: \(error.localizedDescription)"
        }
    }

    func cancel() {
        if !finished, let draft = draftFolder { inbox?.discard(draft: draft) }
        finished = true
        context?.cancelRequest(withError: NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError))
    }
}
