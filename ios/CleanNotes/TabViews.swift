import SwiftUI
import UIKit

// Tab notes: browser tabs CleanTabs saved as notes, shown as previews of
// their web pages, as the desktop app does. In the note, a card above the
// text with the page's picture (large and rounded, or the site's name in
// its place), title and address and "Open in Browser"; tapping the picture
// opens the page too. In the notes list, the site instead of the first line
// and the picture on the right; in the gallery, the picture is the card.
// The core gives each its page (`NoteItem.tab`, `NoteEditor.tabPage()`), and
// its editor holds only the text below the card.

extension TabPage {
    /// The page's address, if it is one.
    var address: URL? { URL(string: url) }

    /// Opens the page in the browser.
    func open() {
        guard let url = address else { return }
        UIApplication.shared.open(url)
    }
}

/// A tab note's picture filling its frame (cut down to its shape), or, with
/// none (or none here yet), the site's name on a soft background.
struct TabPicture: View {
    @EnvironmentObject private var store: NotesStore
    let page: TabPage
    var cornerRadius: CGFloat = 12
    /// The list's thumbnail: just the site's name, small.
    var compact = false

    var body: some View {
        // A picture arriving with a sync shows.
        let _ = store.revision
        let shape = RoundedRectangle(cornerRadius: cornerRadius)
        Group {
            if let name = page.thumbnail, let image = NoteImages.shared.image(name) {
                Color.clear
                    .overlay {
                        Image(uiImage: image)
                            .resizable()
                            .scaledToFill()
                    }
                    .clipped()
            } else {
                ZStack {
                    shape.fill(Color(.secondarySystemFill))
                    VStack(spacing: 6) {
                        if !compact {
                            Image(systemName: "link")
                                .font(.title3)
                                .foregroundStyle(.tertiary)
                        }
                        Text(page.site)
                            .font(compact ? Font.system(size: 8, weight: .semibold) : Font.subheadline.weight(.semibold))
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                            .lineLimit(compact ? 2 : 3)
                    }
                    .padding(compact ? 3 : 10)
                }
            }
        }
        .clipShape(shape)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(page.site)
    }
}

/// The card at the top of a tab note, in the text view above the text (so
/// it scrolls with it): the page's picture, large and rounded (or the site's
/// name on a soft background), its title and address, and "Open in
/// Browser". Tapping the picture opens the page too.
final class TabCardView: UIView {
    var page: TabPage? {
        didSet { update() }
    }

    private let pictureBox = UIView()
    private let picture = UIImageView()
    private let placeholderIcon = UIImageView(image: UIImage(systemName: "link"))
    private let siteLabel = UILabel()
    private let titleLabel = UILabel()
    private let linkIcon = UIImageView(image: UIImage(systemName: "link"))
    private let addressLabel = UILabel()
    let openButton = UIButton(configuration: .filled())
    private let separator = UIView()
    /// The page's picture, if it's here.
    private var image: UIImage?

    private static let rounding: CGFloat = 12

    init() {
        super.init(frame: .zero)
        pictureBox.layer.cornerRadius = Self.rounding
        pictureBox.layer.cornerCurve = .continuous
        pictureBox.clipsToBounds = true
        pictureBox.backgroundColor = .secondarySystemFill
        picture.contentMode = .scaleAspectFill
        picture.clipsToBounds = true
        placeholderIcon.tintColor = .tertiaryLabel
        placeholderIcon.contentMode = .scaleAspectFit
        placeholderIcon.preferredSymbolConfiguration = UIImage.SymbolConfiguration(pointSize: 22, weight: .medium)
        siteLabel.font = .systemFont(ofSize: 20, weight: .semibold)
        siteLabel.textColor = .secondaryLabel
        siteLabel.textAlignment = .center
        siteLabel.numberOfLines = 2
        pictureBox.addSubview(picture)
        pictureBox.addSubview(placeholderIcon)
        pictureBox.addSubview(siteLabel)
        // The picture opens the page, as the button does.
        pictureBox.isUserInteractionEnabled = true
        pictureBox.addGestureRecognizer(UITapGestureRecognizer(target: self, action: #selector(openPage)))
        pictureBox.isAccessibilityElement = true
        pictureBox.accessibilityTraits = .button
        addSubview(pictureBox)

        titleLabel.font = .systemFont(ofSize: 22, weight: .bold)
        titleLabel.textColor = .label
        titleLabel.numberOfLines = 0
        addSubview(titleLabel)

        linkIcon.tintColor = .secondaryLabel
        linkIcon.contentMode = .scaleAspectFit
        linkIcon.preferredSymbolConfiguration = UIImage.SymbolConfiguration(pointSize: 12, weight: .medium)
        addSubview(linkIcon)
        addressLabel.font = .preferredFont(forTextStyle: .subheadline)
        addressLabel.textColor = .secondaryLabel
        addressLabel.lineBreakMode = .byTruncatingTail
        addSubview(addressLabel)

        var config = UIButton.Configuration.filled()
        config.title = "Open in Browser"
        config.image = UIImage(systemName: "safari")
        config.imagePadding = 6
        config.cornerStyle = .medium
        config.baseBackgroundColor = .systemBlue
        config.baseForegroundColor = .white
        openButton.configuration = config
        openButton.addAction(UIAction { [weak self] _ in self?.openPage() }, for: .primaryActionTriggered)
        addSubview(openButton)

        separator.backgroundColor = .separator
        addSubview(separator)
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    private func update() {
        image = page?.thumbnail.flatMap { NoteImages.shared.image($0) }
        picture.image = image
        picture.isHidden = image == nil
        placeholderIcon.isHidden = image != nil
        siteLabel.isHidden = image != nil
        siteLabel.text = page?.site
        titleLabel.text = page?.title
        addressLabel.text = page?.shortUrl
        pictureBox.accessibilityLabel = page.map { "Open \($0.site)" }
        openButton.accessibilityHint = page?.url
        setNeedsLayout()
    }

    @objc private func openPage() {
        page?.open()
    }

    /// As tall as the picture is within reason: about a screenshot's shape
    /// while it's on its way, shorter with none.
    private func pictureHeight(width: CGFloat) -> CGFloat {
        if let image, image.size.width > 0, image.size.height > 0 {
            return min(max(width * image.size.height / image.size.width, 120), 420)
        }
        if page?.thumbnail != nil {
            return min(max(width * 0.56, 120), 360)
        }
        return 150
    }

    private func titleHeight(width: CGFloat) -> CGFloat {
        ceil(titleLabel.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude)).height)
    }

    private var buttonSize: CGSize {
        let s = openButton.intrinsicContentSize
        return CGSize(width: ceil(s.width), height: max(36, ceil(s.height)))
    }

    /// The card's height at `width`.
    func height(for width: CGFloat) -> CGFloat {
        pictureHeight(width: width) + 16 + titleHeight(width: width) + 6 + 20 + 14 + buttonSize.height + 18 + 1
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        let w = bounds.width
        var y: CGFloat = 0
        pictureBox.frame = CGRect(x: 0, y: y, width: w, height: pictureHeight(width: w))
        picture.frame = pictureBox.bounds
        let tall = pictureBox.bounds.height > 100
        placeholderIcon.isHidden = image != nil || !tall
        placeholderIcon.frame = CGRect(x: (w - 28) / 2, y: pictureBox.bounds.midY - 34, width: 28, height: 28)
        siteLabel.frame = CGRect(x: 12, y: tall ? pictureBox.bounds.midY : pictureBox.bounds.midY - 14, width: max(0, w - 24), height: 28)
        y = pictureBox.frame.maxY + 16
        let th = titleHeight(width: w)
        titleLabel.frame = CGRect(x: 0, y: y, width: w, height: th)
        y += th + 6
        linkIcon.frame = CGRect(x: 0, y: y + 2, width: 16, height: 16)
        addressLabel.frame = CGRect(x: 22, y: y, width: max(0, w - 22), height: 20)
        y += 20 + 14
        let b = buttonSize
        openButton.frame = CGRect(x: 0, y: y, width: min(b.width, w), height: b.height)
        y += b.height + 18
        separator.frame = CGRect(x: 0, y: y, width: w, height: 1 / max(1, traitCollection.displayScale))
    }
}

/// CleanTabs' section on the Folders screen (like CleanPassword's, but not
/// locked): an S3 account's saved tabs, synced as a hidden account of their
/// own, with All Tabs, their folders (CleanTabs' collections) and Recently
/// Deleted, each shown as a gallery of websites.
struct TabsSection: View {
    let tabs: [TabsItem]
    let showCount: Bool
    /// The Folders screen's folded sections ("#cleantabs:<id>" for these).
    @Binding var collapsed: Set<String>

    var body: some View {
        ForEach(tabs, id: \.id) { t in
            let key = "#cleantabs:\(t.id)"
            Section(isExpanded: Binding(
                get: { !collapsed.contains(key) },
                set: { open in if open { collapsed.remove(key) } else { collapsed.insert(key) } }
            )) {
                NavigationLink(value: Route.list(.tabs(account: t.id, folder: nil))) {
                    FolderRow(name: "All Tabs", icon: "safari", count: showCount ? Int(t.allCount) : nil)
                }
                ForEach(t.folders) { f in
                    NavigationLink(value: Route.list(.tabs(account: t.id, folder: f.id))) {
                        FolderRow(name: f.name, icon: "folder", count: showCount ? Int(f.count) : nil)
                            .padding(.leading, CGFloat(f.depth) * 18)
                    }
                }
                if t.deletedCount > 0 {
                    NavigationLink(value: Route.list(.tabsDeleted(account: t.id))) {
                        FolderRow(name: "Recently Deleted", icon: "trash", count: showCount ? Int(t.deletedCount) : nil)
                    }
                }
            } header: {
                // With more than one S3 account's tabs, whose these are.
                Text(tabs.count > 1 ? "CleanTabs (\(t.accountName))" : "CleanTabs")
            }
        }
    }
}

/// "1 Tab", "12 Tabs".
func tabsCount(_ n: Int) -> String {
    n == 1 ? "1 Tab" : "\(n) Tabs"
}
