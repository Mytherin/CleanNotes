import ImageIO
import PDFKit
import PhotosUI
import QuickLook
import SwiftUI
import UIKit
import UniformTypeIdentifiers

// The note editor: a UITextView whose text is the shared editing engine's
// document (the same one the desktop editor drives). Every edit goes to the
// engine first. Plain typing is left to the text view (so autocorrect,
// dictation and other keyboards work as everywhere); edits that change
// paragraphs (Return in a list, "- " starting one, Backspace taking a list
// style off, the Aa panel's styles) are made by the engine and the note is
// shown again. List markers, checklist circles, quote bars and code blocks
// are drawn in the margin, as in Apple Notes.

extension NSAttributedString.Key {
    /// A paragraph's style ([`ParagraphMark`]) on all its characters.
    static let cnParagraph = NSAttributedString.Key("CleanNotesParagraph")
    /// A #tag.
    static let cnTag = NSAttributedString.Key("CleanNotesTag")
}

/// A paragraph's style, for drawing its marker.
final class ParagraphMark: NSObject {
    let style: ParagraphStyle
    let indent: Int
    let checked: Bool
    /// The core's marker ("•", "–", "3.", "☐"); empty if not a list item.
    let marker: String
    /// A table cell: where it is in its table.
    let cell: TableCell?
    /// An attached file on a line of its own: drawn as a card.
    let file: AttachedFile?

    init(_ p: Paragraph) {
        style = p.style
        indent = Int(p.indent)
        checked = p.checked
        marker = p.marker
        cell = p.cell
        file = p.file
    }

    var isList: Bool { !marker.isEmpty }
}

/// Pictures in notes: loaded once (no bigger than the screen needs), by
/// attachment name. A picture not on the phone yet isn't remembered, so it
/// shows once a sync brings it.
final class NoteImages {
    static let shared = NoteImages()
    private let cache = NSCache<NSString, UIImage>()
    /// An attachment's bytes (the core reads them, and decrypts a locked
    /// note's in memory), if it's here.
    var data: (String) -> Data? = { _ in nil }

    func image(_ name: String) -> UIImage? {
        if let i = cache.object(forKey: name as NSString) { return i }
        guard let d = data(name), let src = CGImageSourceCreateWithData(d as CFData, nil) else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 2048,
        ]
        guard let cg = CGImageSourceCreateThumbnailAtIndex(src, 0, options as CFDictionary) else { return nil }
        let img = UIImage(cgImage: cg)
        cache.setObject(img, forKey: name as NSString)
        return img
    }

    /// Locked notes closed: their pictures, decrypted, aren't kept.
    func forgetAll() {
        cache.removeAllObjects()
        pdfs.removeAllObjects()
        pages.removeAllObjects()
        sizes.removeAll()
    }

    // ---- Attached files (PDFs' pages, for their cards) ----

    private let pdfs = NSCache<NSString, PDFDocument>()
    private let pages = NSCache<NSString, UIImage>()
    private var sizes: [String: Int] = [:]

    /// An attached PDF, if it's here and can be read (not an encrypted
    /// one, say).
    func pdf(_ reference: String) -> PDFDocument? {
        if let d = pdfs.object(forKey: reference as NSString) { return d }
        guard let data = data(reference) else { return nil }
        sizes[reference] = data.count
        guard let d = PDFDocument(data: data), !d.isLocked, d.pageCount > 0 else { return nil }
        pdfs.setObject(d, forKey: reference as NSString)
        return d
    }

    /// An attached file's size in bytes, if it's here.
    func size(_ reference: String) -> Int? {
        if let n = sizes[reference] { return n }
        guard let data = data(reference) else { return nil }
        sizes[reference] = data.count
        return data.count
    }

    /// A PDF page's size in points, as it's shown (turned, if it is).
    static func pageSize(_ page: PDFPage) -> CGSize {
        let b = page.bounds(for: .cropBox).size
        return page.rotation % 180 == 0 ? b : CGSize(width: b.height, height: b.width)
    }

    /// Page `index` of an attached PDF drawn on white, `height` points
    /// high (sharp on this screen).
    func page(_ reference: String, index: Int, height: CGFloat, scale: CGFloat) -> UIImage? {
        let px = Int((height * scale).rounded())
        let key = "\(reference):\(index):\(px)" as NSString
        if let i = pages.object(forKey: key) { return i }
        guard px > 0, let page = pdf(reference)?.page(at: index) else { return nil }
        let size = Self.pageSize(page)
        guard size.width > 0, size.height > 0 else { return nil }
        let k = CGFloat(px) / size.height
        let img = page.thumbnail(of: CGSize(width: size.width * k, height: CGFloat(px)), for: .cropBox)
        pages.setObject(img, forKey: key)
        return img
    }

    /// A box shown while a picture is on its way.
    static func placeholder(width: CGFloat) -> UIImage {
        let size = CGSize(width: width, height: 150)
        return UIGraphicsImageRenderer(size: size).image { _ in
            let path = UIBezierPath(roundedRect: CGRect(origin: .zero, size: size).insetBy(dx: 0.5, dy: 0.5), cornerRadius: 8)
            UIColor.secondarySystemFill.setFill()
            path.fill()
            UIColor.separator.setStroke()
            path.stroke()
            let text = "Picture not here yet" as NSString
            let attrs: [NSAttributedString.Key: Any] = [.font: UIFont.preferredFont(forTextStyle: .footnote), .foregroundColor: UIColor.secondaryLabel]
            let t = text.size(withAttributes: attrs)
            text.draw(at: CGPoint(x: (size.width - t.width) / 2, y: (size.height - t.height) / 2), withAttributes: attrs)
        }
    }

    /// A picture's bytes as they're best kept: PNG and GIF as they are;
    /// photos (HEIC, JPEG) drawn upright, at most 3000 pixels, as JPEG, so
    /// every device shows them (the desktop app can't read HEIC).
    static func portable(_ data: Data) -> Data {
        if data.starts(with: [0x89, 0x50, 0x4E, 0x47]) || data.starts(with: Array("GIF8".utf8)) {
            return data
        }
        guard let img = UIImage(data: data) else { return data }
        let px = CGSize(width: img.size.width * img.scale, height: img.size.height * img.scale)
        let k = min(1, 3000 / max(px.width, px.height, 1))
        let size = CGSize(width: (px.width * k).rounded(), height: (px.height * k).rounded())
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = true
        let upright = UIGraphicsImageRenderer(size: size, format: format).image { _ in
            img.draw(in: CGRect(origin: .zero, size: size))
        }
        return upright.jpegData(compressionQuality: 0.85) ?? data
    }
}

/// How wide pictures may be shown, and the screen's scale.
struct ImageLayout {
    var width: CGFloat
    var scale: CGFloat

    static let standard = ImageLayout(width: 300, scale: 2)
}

/// Turns the engine's paragraphs into attributed text.
enum NoteTextRenderer {
    /// Body text size on the iPhone for the settings' text size (the
    /// desktop's 15 pt default is the iPhone's 17 pt).
    static func bodySize(_ textSize: Float) -> CGFloat {
        CGFloat(textSize) * 17 / 15
    }

    static func indentStep(_ body: CGFloat) -> CGFloat { body * 26 / 15 }
    static func markerWidth(_ style: ParagraphStyle, body: CGFloat) -> CGFloat {
        style == .checklist ? body * 1.9 : body * 1.6
    }
    static func quoteWidth(_ body: CGFloat) -> CGFloat { body }

    static func attributed(_ paragraphs: [Paragraph], textSize: Float, images: ImageLayout = .standard) -> NSAttributedString {
        let out = NSMutableAttributedString()
        for (i, p) in paragraphs.enumerated() {
            out.append(attributed(p, textSize: textSize, terminated: i + 1 < paragraphs.count, images: images))
        }
        return out
    }

    /// One paragraph, with its line break if another follows.
    static func attributed(_ p: Paragraph, textSize: Float, terminated: Bool, images: ImageLayout = .standard, cards: Bool = true) -> NSAttributedString {
        let out = NSMutableAttributedString()
        let mark = ParagraphMark(p)
        let para = paragraphStyle(p, textSize: textSize, width: images.width, cards: cards)
        for run in p.runs {
            let attrs = attributes(p, run: run, textSize: textSize, mark: mark, para: para)
            if let name = run.image {
                // One attachment character, as in the engine's text.
                let picture = NSMutableAttributedString(attachment: attachment(name, layout: images))
                picture.addAttributes(attrs, range: NSRange(location: 0, length: picture.length))
                out.append(picture)
            } else {
                out.append(NSAttributedString(string: run.text, attributes: attrs))
            }
        }
        if terminated {
            out.append(NSAttributedString(string: "\n", attributes: attributes(p, run: nil, textSize: textSize, mark: mark, para: para)))
        }
        return out
    }

    /// A picture, as wide as it is (in points) but no wider than the text.
    static func attachment(_ name: String, layout: ImageLayout) -> NSTextAttachment {
        let a = NSTextAttachment()
        let width = max(layout.width, 60)
        if let img = NoteImages.shared.image(name), img.size.width > 0 {
            var size = CGSize(width: img.size.width / max(layout.scale, 1), height: img.size.height / max(layout.scale, 1))
            if size.width > width {
                size = CGSize(width: width, height: size.height * width / size.width)
            }
            a.image = img
            a.bounds = CGRect(origin: .zero, size: size)
        } else {
            let w = min(width, 260)
            a.image = NoteImages.placeholder(width: w)
            a.bounds = CGRect(x: 0, y: 0, width: w, height: 150)
        }
        return a
    }

    /// Space between a table cell's box and its text.
    static let cellPadding: CGFloat = 12

    /// `width`: the text's (a large PDF's pages are as wide); `cards`:
    /// attached files are drawn as cards (not when printing), room kept
    /// around their names for them.
    static func paragraphStyle(_ p: Paragraph, textSize: Float, width: CGFloat = ImageLayout.standard.width, cards: Bool = true) -> NSParagraphStyle {
        let body = bodySize(textSize)
        let s = NSMutableParagraphStyle()
        if p.cell != nil {
            // A table cell: its text inset in the box drawn around it.
            s.firstLineHeadIndent = cellPadding
            s.headIndent = cellPadding
            s.tailIndent = -cellPadding
            s.paragraphSpacingBefore = body * 0.4
            s.paragraphSpacing = body * 0.4
            s.lineSpacing = body * 0.08
            return s
        }
        var indent = CGFloat(p.indent) * indentStep(body)
        let mark = ParagraphMark(p)
        if mark.isList {
            indent += markerWidth(p.style, body: body)
        } else if p.style == .quote {
            indent += quoteWidth(body)
        }
        s.firstLineHeadIndent = indent
        s.headIndent = indent
        switch p.style {
        case .title: s.paragraphSpacingBefore = body * 0.3
        case .heading: s.paragraphSpacingBefore = body * 0.45
        case .subheading: s.paragraphSpacingBefore = body * 0.3
        case .checklist: s.paragraphSpacingBefore = body * 0.15
        default: break
        }
        s.lineSpacing = p.style == .monospaced ? 0 : body * 0.08
        if cards, let file = p.file {
            FileCards.reserve(file, in: s, width: width - indent)
        }
        return s
    }

    static func font(_ style: ParagraphStyle, run: TextRun?, textSize: Float) -> UIFont {
        let body = bodySize(textSize)
        let size = CGFloat(fontSize(style: style, body: Float(body)))
        if style == .monospaced || run?.code == true {
            return UIFont.monospacedSystemFont(ofSize: style == .monospaced ? size : body * 0.93, weight: run?.bold == true ? .bold : .regular)
        }
        let weight: UIFont.Weight
        switch style {
        case .title: weight = .bold
        case .heading: weight = .bold
        case .subheading: weight = .semibold
        default: weight = .regular
        }
        var font = UIFont.systemFont(ofSize: size, weight: weight)
        var traits: UIFontDescriptor.SymbolicTraits = []
        if run?.bold == true { traits.insert(.traitBold) }
        if run?.italic == true { traits.insert(.traitItalic) }
        if !traits.isEmpty, let d = font.fontDescriptor.withSymbolicTraits(font.fontDescriptor.symbolicTraits.union(traits)) {
            font = UIFont(descriptor: d, size: size)
        }
        return font
    }

    static func attributes(_ p: Paragraph, run: TextRun?, textSize: Float, mark: ParagraphMark? = nil, para: NSParagraphStyle? = nil) -> [NSAttributedString.Key: Any] {
        var a: [NSAttributedString.Key: Any] = [
            .font: font(p.style, run: run, textSize: textSize),
            .foregroundColor: UIColor.label,
            .paragraphStyle: para ?? paragraphStyle(p, textSize: textSize),
            .cnParagraph: mark ?? ParagraphMark(p),
        ]
        // A table's header cells are bold.
        if p.cell?.header == true, let f = a[.font] as? UIFont,
           let d = f.fontDescriptor.withSymbolicTraits(f.fontDescriptor.symbolicTraits.union(.traitBold)) {
            a[.font] = UIFont(descriptor: d, size: f.pointSize)
        }
        guard let run else { return a }
        if run.underline { a[.underlineStyle] = NSUnderlineStyle.single.rawValue }
        if run.strike { a[.strikethroughStyle] = NSUnderlineStyle.single.rawValue }
        if let link = run.link, let url = URL(string: link) {
            a[.link] = url
        }
        if run.tag {
            a[.foregroundColor] = UIColor.notesAccent
            a[.cnTag] = true
        }
        if p.checked && p.style == .checklist {
            // Ticked items fade, as in Apple Notes.
            a[.foregroundColor] = UIColor.secondaryLabel
        }
        return a
    }

    /// The note for printing: list markers and checkboxes as text (on screen
    /// they're drawn in the margin), in light mode's colors.
    static func printable(_ paragraphs: [Paragraph], textSize: Float) -> NSAttributedString {
        let out = NSMutableAttributedString()
        for (i, p) in paragraphs.enumerated() {
            let line = NSMutableAttributedString(attributedString: attributed(p, textSize: textSize, terminated: i + 1 < paragraphs.count, cards: false))
            let marker = p.marker
            if !marker.isEmpty, let para = paragraphStyle(p, textSize: textSize).mutableCopy() as? NSMutableParagraphStyle {
                // The marker hangs to the left of the text, as on screen.
                para.firstLineHeadIndent = CGFloat(p.indent) * indentStep(bodySize(textSize))
                para.tabStops = [NSTextTab(textAlignment: .left, location: para.headIndent)]
                var attrs = attributes(p, run: nil, textSize: textSize)
                attrs[.paragraphStyle] = para
                attrs[.foregroundColor] = UIColor.label
                line.insert(NSAttributedString(string: marker + "\t", attributes: attrs), at: 0)
                line.addAttribute(.paragraphStyle, value: para, range: NSRange(location: 0, length: line.length))
            }
            out.append(line)
        }
        let light = UITraitCollection(userInterfaceStyle: .light)
        let all = NSRange(location: 0, length: out.length)
        out.enumerateAttribute(.foregroundColor, in: all) { value, range, _ in
            if let color = value as? UIColor {
                out.addAttribute(.foregroundColor, value: color.resolvedColor(with: light), range: range)
            }
        }
        return out
    }

    /// What typing at the caret should look like.
    static func typingAttributes(_ p: Paragraph, formatting f: Formatting, textSize: Float) -> [NSAttributedString.Key: Any] {
        let run = TextRun(text: "", bold: f.bold, italic: f.italic, underline: f.underline, strike: f.strike, code: f.code, link: f.link, tag: false, image: nil)
        return attributes(p, run: run, textSize: textSize)
    }
}

/// Attached files' cards, as in Apple Notes: the file's name (the
/// paragraph's text, still edited as text) in a rounded box, and below it
/// a PDF's pages side by side (Medium, as usual) or its first page as wide
/// as the note (Large); Small, and any other file, is the name with "PDF
/// Document · 211 KB" below it and a small picture of it on the right.
/// Room is kept with the paragraph's spacing and indents (the text stays
/// the engine's); [`NotesLayoutManager`] draws the rest. A tap on the card
/// opens the file in Quick Look.
enum FileCards {
    /// Inside the card, around its contents.
    static let pad: CGFloat = 12
    /// Between cards and the text around them.
    static let margin: CGFloat = 6
    /// Between the name and the pages below it.
    static let gap: CGFloat = 8
    /// A Medium card's pages' height.
    static let pagesHeight: CGFloat = 200
    /// A Small card's picture.
    static let thumb = CGSize(width: 40, height: 52)
    static var detailsFont: UIFont { .preferredFont(forTextStyle: .footnote) }

    /// How it's shown here: a PDF that can't be read (not on this device
    /// yet) is Small, as other files are.
    static func view(_ f: AttachedFile) -> FileView {
        f.pdf && f.view != .small && NoteImages.shared.pdf(f.reference) != nil ? f.view : .small
    }

    /// Keeps room in its paragraph's style for the card of `f`, `width`
    /// wide.
    static func reserve(_ f: AttachedFile, in s: NSMutableParagraphStyle, width: CGFloat) {
        s.firstLineHeadIndent += pad
        s.headIndent += pad
        s.paragraphSpacingBefore = margin + pad
        let below: CGFloat
        switch view(f) {
        case .small:
            s.tailIndent = -(pad + thumb.width + gap)
            below = max(detailsFont.lineHeight + 2, 0)
        case .medium:
            s.tailIndent = -pad
            below = gap + pagesHeight
        case .large:
            s.tailIndent = -pad
            below = gap + largeHeight(f, width: width - 2 * pad)
        }
        s.paragraphSpacing = below + pad + margin
    }

    /// A Large card's first page's height, `width` wide (no taller than
    /// a screenful).
    static func largeHeight(_ f: AttachedFile, width: CGFloat) -> CGFloat {
        guard let page = NoteImages.shared.pdf(f.reference)?.page(at: 0) else { return pagesHeight }
        let size = NoteImages.pageSize(page)
        guard size.width > 0 else { return pagesHeight }
        return min(max(width, 60) * size.height / size.width, 600)
    }

    /// "PDF Document · 211 KB".
    static func details(_ f: AttachedFile, label: String) -> String {
        let ext = (label.trimmingCharacters(in: .whitespacesAndNewlines) as NSString).pathExtension.uppercased()
        let kind = f.pdf ? "PDF Document" : ext.isEmpty ? "Document" : "\(ext) Document"
        guard let n = NoteImages.shared.size(f.reference) else { return "\(kind) · Not on this device yet" }
        return "\(kind) · \(ByteCountFormatter.string(fromByteCount: Int64(n), countStyle: .file))"
    }
}

/// Draws list markers, checklist circles, quote bars and code block
/// backgrounds next to the text, and attached files' cards.
final class NotesLayoutManager: NSLayoutManager {
    var textSize: Float = 15
    /// The last paragraph's style when it's empty (it has no characters to
    /// carry it).
    var trailingMark: ParagraphMark?

    override func drawBackground(forGlyphRange glyphsToShow: NSRange, at origin: CGPoint) {
        super.drawBackground(forGlyphRange: glyphsToShow, at: origin)
        guard let storage = textStorage, storage.length > 0 || trailingMark != nil else {
            return
        }
        let body = NoteTextRenderer.bodySize(textSize)
        let text = storage.string as NSString
        let chars = characterRange(forGlyphRange: glyphsToShow, actualGlyphRange: nil)
        let range = text.paragraphRange(for: NSRange(location: chars.location, length: max(chars.length, 0)))
        if range.length > 0 {
            storage.enumerateAttribute(.cnParagraph, in: range) { value, r, _ in
                guard let mark = value as? ParagraphMark else { return }
                let rects = lineRects(for: r, origin: origin)
                guard let first = rects.first else { return }
                if let file = mark.file {
                    drawFileCard(file, chars: r, rects: rects, origin: origin)
                }
                if let cell = mark.cell {
                    drawCell(cell, rects: rects, origin: origin)
                    return
                }
                switch mark.style {
                case .quote:
                    let x = origin.x + CGFloat(mark.indent) * NoteTextRenderer.indentStep(body) + 2
                    let bar = CGRect(x: x, y: first.minY, width: 3, height: (rects.last?.maxY ?? first.maxY) - first.minY)
                    UIColor.tertiaryLabel.setFill()
                    UIBezierPath(roundedRect: bar, cornerRadius: 1.5).fill()
                case .monospaced:
                    let union = rects.dropFirst().reduce(first) { $0.union($1) }
                    let bg = CGRect(x: origin.x - 4, y: union.minY - 1, width: (textContainers.first?.size.width ?? union.width) - 4, height: union.height + 2)
                    UIColor.secondarySystemFill.setFill()
                    UIBezierPath(rect: bg).fill()
                default:
                    break
                }
                // The marker goes on a paragraph's first line only.
                let startsParagraph = r.location == 0 || text.character(at: r.location - 1) == 10
                if mark.isList && startsParagraph {
                    let glyph = glyphIndexForCharacter(at: r.location)
                    let baseline = first.minY + location(forGlyphAt: glyph).y
                    drawMarker(mark, lineTop: first.minY, baseline: baseline, origin: origin, body: body)
                }
            }
        }
        // An empty last paragraph is drawn on the extra line.
        if let mark = trailingMark, mark.isList, !extraLineFragmentRect.isEmpty {
            let r = extraLineFragmentRect.offsetBy(dx: origin.x, dy: origin.y)
            let font = NoteTextRenderer.font(mark.style, run: nil, textSize: textSize)
            drawMarker(mark, lineTop: r.minY, baseline: r.minY + font.ascender + NoteTextRenderer.paragraphStyleSpacing(mark, body: body), origin: origin, body: body)
        }
    }

    /// A table cell's box. The phone is too narrow for the table's columns
    /// side by side: each row is a box with its cells one under another,
    /// a faint line between them and a solid one between rows; header
    /// cells are shaded.
    private func drawCell(_ cell: TableCell, rects: [CGRect], origin: CGPoint) {
        guard let first = rects.first else { return }
        let width = textContainers.first?.size.width ?? first.width
        let bottom = rects.last?.maxY ?? first.maxY
        let box = CGRect(x: origin.x, y: first.minY, width: width, height: bottom - first.minY)
        if cell.header {
            UIColor.secondarySystemFill.setFill()
            UIRectFill(box)
        }
        let line: CGFloat = 1
        UIColor.separator.setFill()
        UIRectFill(CGRect(x: box.minX, y: box.minY, width: line, height: box.height))
        UIRectFill(CGRect(x: box.maxX - line, y: box.minY, width: line, height: box.height))
        if cell.column == 0 {
            UIRectFill(CGRect(x: box.minX, y: box.minY, width: box.width, height: line))
        } else {
            let inset = NoteTextRenderer.cellPadding
            UIColor.separator.withAlphaComponent(0.35).setFill()
            UIRectFill(CGRect(x: box.minX + inset, y: box.minY, width: box.width - 2 * inset, height: 0.5))
            UIColor.separator.setFill()
        }
        if cell.row + 1 == cell.rows && cell.column + 1 == cell.columns {
            UIRectFill(CGRect(x: box.minX, y: box.maxY - line, width: box.width, height: line))
        }
    }

    /// Where an attached file's card is, from the line fragments its
    /// paragraph `r` is laid out on (view coordinates); and the bottom of
    /// its name's text, and where its text starts.
    func fileCardFrame(chars r: NSRange, rects: [CGRect], origin: CGPoint) -> (card: CGRect, nameBottom: CGFloat, textX: CGFloat)? {
        guard let first = rects.first, let last = rects.last, let storage = textStorage, r.length > 0 else { return nil }
        let para = storage.attribute(.paragraphStyle, at: r.location, effectiveRange: nil) as? NSParagraphStyle
        let padding = textContainers.first?.lineFragmentPadding ?? 0
        let textX = first.minX + padding + (para?.headIndent ?? 0)
        let pad = FileCards.pad
        let top = lineFragmentUsedRect(forGlyphAt: glyphIndexForCharacter(at: r.location), effectiveRange: nil).minY + origin.y
        let nameBottom = lineFragmentUsedRect(forGlyphAt: glyphIndexForCharacter(at: NSMaxRange(r) - 1), effectiveRange: nil).maxY + origin.y
        let bottom = max(last.maxY - FileCards.margin, nameBottom + pad)
        let left = textX - pad
        let card = CGRect(x: left, y: max(top - pad, first.minY), width: first.maxX - padding - left, height: bottom - max(top - pad, first.minY))
        return (card, nameBottom, textX)
    }

    /// An attached file's card (see [`FileCards`]) behind its name.
    private func drawFileCard(_ f: AttachedFile, chars r: NSRange, rects: [CGRect], origin: CGPoint) {
        guard let (card, nameBottom, textX) = fileCardFrame(chars: r, rects: rects, origin: origin) else { return }
        let pad = FileCards.pad
        let box = UIBezierPath(roundedRect: card, cornerRadius: 12)
        UIColor.secondarySystemFill.setFill()
        box.fill()
        UIColor.separator.setStroke()
        box.lineWidth = 0.5
        box.stroke()
        let scale = max(UITraitCollection.current.displayScale, 1)
        switch FileCards.view(f) {
        case .small:
            let label = (textStorage?.string as NSString?)?.substring(with: r).trimmingCharacters(in: .newlines) ?? ""
            let style = NSMutableParagraphStyle()
            style.lineBreakMode = .byTruncatingTail
            let attrs: [NSAttributedString.Key: Any] = [.font: FileCards.detailsFont, .foregroundColor: UIColor.secondaryLabel, .paragraphStyle: style]
            let thumbX = card.maxX - pad - FileCards.thumb.width
            let details = CGRect(x: textX, y: nameBottom + 1, width: max(thumbX - FileCards.gap - textX, 0), height: FileCards.detailsFont.lineHeight + 1)
            (FileCards.details(f, label: label) as NSString).draw(with: details, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine], attributes: attrs, context: nil)
            let spot = CGRect(x: thumbX, y: card.midY - FileCards.thumb.height / 2, width: FileCards.thumb.width, height: FileCards.thumb.height)
            if f.pdf, let page = NoteImages.shared.pdf(f.reference)?.page(at: 0) {
                let size = NoteImages.pageSize(page)
                let k = min(spot.width / max(size.width, 1), spot.height / max(size.height, 1))
                let fit = CGRect(x: spot.midX - size.width * k / 2, y: spot.midY - size.height * k / 2, width: size.width * k, height: size.height * k)
                drawPage(NoteImages.shared.page(f.reference, index: 0, height: fit.height, scale: scale), in: fit)
            } else {
                let symbol = UIImage(systemName: "doc", withConfiguration: UIImage.SymbolConfiguration(pointSize: 30, weight: .light))?
                    .withTintColor(.secondaryLabel, renderingMode: .alwaysOriginal)
                if let symbol {
                    symbol.draw(at: CGPoint(x: spot.midX - symbol.size.width / 2, y: spot.midY - symbol.size.height / 2))
                }
            }
        case .medium, .large:
            guard let doc = NoteImages.shared.pdf(f.reference) else { return }
            let area = CGRect(x: textX, y: nameBottom + FileCards.gap, width: card.maxX - pad - textX, height: card.maxY - pad - nameBottom - FileCards.gap)
            guard area.width > 0, area.height > 0, let ctx = UIGraphicsGetCurrentContext() else { return }
            ctx.saveGState()
            ctx.clip(to: area.insetBy(dx: -1, dy: -1))
            if FileCards.view(f) == .large {
                if let page = doc.page(at: 0) {
                    let size = NoteImages.pageSize(page)
                    let k = min(area.width / max(size.width, 1), area.height / max(size.height, 1))
                    let fit = CGRect(x: area.midX - size.width * k / 2, y: area.minY, width: size.width * k, height: size.height * k)
                    drawPage(NoteImages.shared.page(f.reference, index: 0, height: fit.height, scale: scale), in: fit)
                }
            } else {
                // As many as fit, side by side (the last one cut off when
                // there are more).
                var x = area.minX
                for i in 0..<doc.pageCount {
                    guard x < area.maxX, let page = doc.page(at: i) else { break }
                    let size = NoteImages.pageSize(page)
                    let w = area.height * size.width / max(size.height, 1)
                    let rect = CGRect(x: x, y: area.minY, width: w, height: area.height)
                    drawPage(NoteImages.shared.page(f.reference, index: i, height: area.height, scale: scale), in: rect)
                    x += w + FileCards.gap
                }
            }
            ctx.restoreGState()
        }
    }

    /// A page of a PDF on white, with a hairline round it.
    private func drawPage(_ image: UIImage?, in rect: CGRect) {
        UIColor.white.setFill()
        UIRectFill(rect)
        image?.draw(in: rect)
        UIColor.separator.setStroke()
        let edge = UIBezierPath(rect: rect.insetBy(dx: 0.25, dy: 0.25))
        edge.lineWidth = 0.5
        edge.stroke()
    }

    /// The line fragments a character range is laid out on (view
    /// coordinates).
    private func lineRects(for chars: NSRange, origin: CGPoint) -> [CGRect] {
        var out: [CGRect] = []
        let glyphs = glyphRange(forCharacterRange: chars, actualCharacterRange: nil)
        enumerateLineFragments(forGlyphRange: glyphs) { rect, _, _, _, _ in
            out.append(rect.offsetBy(dx: origin.x, dy: origin.y))
        }
        return out
    }

    private func drawMarker(_ mark: ParagraphMark, lineTop: CGFloat, baseline: CGFloat, origin: CGPoint, body: CGFloat) {
        let font = NoteTextRenderer.font(.body, run: nil, textSize: textSize)
        let x0 = origin.x + CGFloat(mark.indent) * NoteTextRenderer.indentStep(body)
        let width = NoteTextRenderer.markerWidth(mark.style, body: body)
        switch mark.style {
        case .checklist:
            let d = body * 1.3
            let rect = CGRect(x: x0, y: baseline - font.capHeight / 2 - d / 2, width: d, height: d)
            let config = UIImage.SymbolConfiguration(pointSize: d, weight: .light)
            let image: UIImage?
            if mark.checked {
                image = UIImage(systemName: "checkmark.circle.fill", withConfiguration: config.applying(UIImage.SymbolConfiguration(paletteColors: [.white, .notesAccent])))
            } else {
                image = UIImage(systemName: "circle", withConfiguration: config)?.withTintColor(.tertiaryLabel, renderingMode: .alwaysOriginal)
            }
            image?.draw(in: rect)
        default:
            let attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: UIColor.label]
            let s = NSAttributedString(string: mark.marker, attributes: attrs)
            let size = s.size()
            // Numbers line up on their right, bullets sit in the middle.
            let x = mark.style == .numbered ? x0 + width - size.width - body * 0.35 : x0 + (width - size.width) / 2 - body * 0.2
            s.draw(at: CGPoint(x: x, y: baseline - font.ascender))
        }
    }
}

extension NoteTextRenderer {
    static func paragraphStyleSpacing(_ mark: ParagraphMark, body: CGFloat) -> CGFloat {
        mark.style == .checklist ? body * 0.15 : 0
    }
}

/// Undo and redo go to the engine (shake to undo, ⌘Z, the undo buttons).
final class EngineUndoManager: UndoManager {
    weak var coordinator: NoteEditorView.Coordinator?

    override init() {
        super.init()
        // The text view's own undo steps would be out of step with the
        // engine's: they're not kept.
        disableUndoRegistration()
    }

    override var canUndo: Bool { coordinator?.formatting?.canUndo ?? false }
    override var canRedo: Bool { coordinator?.formatting?.canRedo ?? false }
    override func undo() { coordinator?.command(.undo) }
    override func redo() { coordinator?.command(.redo) }
    override var undoActionName: String { "Typing" }
    override var redoActionName: String { "Typing" }
}

/// The text view: copy and paste keep formatting within the app, B/I/U
/// (menu and ⌘B ⌘I ⌘U) go to the engine, and undo is the engine's.
final class NoteTextView: UITextView {
    /// Text storage isn't kept by the layout manager: kept here.
    let storage: NSTextStorage
    let notesLayout: NotesLayoutManager
    let engineUndo = EngineUndoManager()
    weak var coordinator: NoteEditorView.Coordinator?
    /// The date above the note.
    let dateLabel = UILabel()

    init() {
        storage = NSTextStorage()
        notesLayout = NotesLayoutManager()
        storage.addLayoutManager(notesLayout)
        let container = NSTextContainer(size: CGSize(width: 0, height: CGFloat.greatestFiniteMagnitude))
        container.widthTracksTextView = true
        notesLayout.addTextContainer(container)
        super.init(frame: .zero, textContainer: container)
        dateLabel.font = .preferredFont(forTextStyle: .caption1)
        dateLabel.textColor = .secondaryLabel
        dateLabel.textAlignment = .center
        addSubview(dateLabel)
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    override var undoManager: UndoManager? { engineUndo }

    /// The width pictures were last fitted to.
    private var laidOutWidth: CGFloat = 0

    override func layoutSubviews() {
        super.layoutSubviews()
        dateLabel.frame = CGRect(x: 0, y: 6, width: bounds.width, height: 18)
        // Pictures fit the text's width: shown again when it changes (the
        // first layout, turning the phone).
        if abs(bounds.width - laidOutWidth) > 0.5 {
            laidOutWidth = bounds.width
            coordinator?.widthChanged()
        }
    }

    override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
        if action == #selector(paste(_:)) && UIPasteboard.general.hasImages && !UIPasteboard.general.hasStrings {
            return !(coordinator?.readOnly ?? true)
        }
        return super.canPerformAction(action, withSender: sender)
    }

    override func copy(_ sender: Any?) {
        guard let c = coordinator else { return super.copy(sender) }
        if let text = c.editor.copy() { UIPasteboard.general.string = text }
    }

    override func cut(_ sender: Any?) {
        guard let c = coordinator, !c.readOnly else { return }
        if let text = c.editor.cut() {
            UIPasteboard.general.string = text
            c.render()
            c.edited()
        }
    }

    override func paste(_ sender: Any?) {
        guard let c = coordinator else { return super.paste(sender) }
        if c.readOnly { c.blockedEdit(); return }
        let pb = UIPasteboard.general
        if !pb.hasStrings && pb.hasImages {
            // A picture: attached.
            let data = pb.data(forPasteboardType: UTType.png.identifier)
                ?? pb.data(forPasteboardType: UTType.jpeg.identifier)
                ?? pb.data(forPasteboardType: UTType.heic.identifier)
                ?? pb.image?.pngData()
            if let data { c.attachImage(data) }
            return
        }
        guard let text = pb.string else { return }
        c.editor.paste(text: text)
        c.render()
        c.edited()
    }

    override func toggleBoldface(_ sender: Any?) { coordinator?.command(.bold) }
    override func toggleItalics(_ sender: Any?) { coordinator?.command(.italic) }
    override func toggleUnderline(_ sender: Any?) { coordinator?.command(.underline) }

    // ---- Keyboard shortcuts (Apple Notes' on iPad, and the desktop's) ----

    private static let shortcuts: [(title: String, input: String, flags: UIKeyModifierFlags, id: String)] = [
        ("Title", "t", [.command, .shift], "title"),
        ("Heading", "h", [.command, .shift], "heading"),
        ("Subheading", "j", [.command, .shift], "subheading"),
        ("Body", "b", [.command, .shift], "body"),
        ("Monospaced", "m", [.command, .shift], "monospaced"),
        ("Bulleted List", "7", [.command, .shift], "bulleted"),
        ("Dashed List", "8", [.command, .shift], "dashed"),
        ("Numbered List", "9", [.command, .shift], "numbered"),
        ("Checklist", "l", [.command, .shift], "checklist"),
        ("Mark as Checked", "u", [.command, .shift], "check"),
        ("Block Quote", "'", [.command], "quote"),
        ("Indent", "]", [.command], "indent"),
        ("Outdent", "[", [.command], "outdent"),
        ("Add Link", "k", [.command], "link"),
        ("Move Up", UIKeyCommand.inputUpArrow, [.command, .control], "up"),
        ("Move Down", UIKeyCommand.inputDownArrow, [.command, .control], "down"),
        ("Table", "t", [.command, .alternate], "table"),
    ]

    override var keyCommands: [UIKeyCommand]? {
        guard let c = coordinator, !c.readOnly else { return super.keyCommands }
        var mine = Self.shortcuts.map { s in
            UIKeyCommand(title: s.title, action: #selector(shortcut(_:)), input: s.input, modifierFlags: s.flags, propertyList: s.id)
        }
        // In a table, Tab and Shift+Tab go from cell to cell.
        if c.formatting?.inTable == true {
            for (title, flags, id) in [("Next Cell", UIKeyModifierFlags(), "nextCell"), ("Previous Cell", UIKeyModifierFlags.shift, "previousCell")] {
                let k = UIKeyCommand(title: title, action: #selector(shortcut(_:)), input: "\t", modifierFlags: flags, propertyList: id)
                k.wantsPriorityOverSystemBehavior = true
                mine.append(k)
            }
        }
        return (super.keyCommands ?? []) + mine
    }

    @objc private func shortcut(_ sender: UIKeyCommand) {
        guard let c = coordinator, let id = sender.propertyList as? String else { return }
        let command: EditorCommand
        switch id {
        case "title": command = .setStyle(style: .title)
        case "heading": command = .setStyle(style: .heading)
        case "subheading": command = .setStyle(style: .subheading)
        case "body": command = .setStyle(style: .body)
        case "monospaced": command = .setStyle(style: .monospaced)
        case "bulleted": command = .setStyle(style: .bulleted)
        case "dashed": command = .setStyle(style: .dashed)
        case "numbered": command = .setStyle(style: .numbered)
        case "checklist": command = .setStyle(style: .checklist)
        case "check": command = .toggleChecklist
        case "quote": command = .setStyle(style: c.formatting?.style == .quote ? .body : .quote)
        case "indent": command = .indent
        case "outdent": command = .outdent
        case "up": command = .moveUp
        case "down": command = .moveDown
        case "table": command = c.formatting?.inTable == true ? .nextCell : .insertTable
        case "nextCell": command = .nextCell
        case "previousCell": command = .previousCell
        case "link":
            c.onLink()
            return
        default: return
        }
        c.command(command)
    }
}

/// A checklist's actions (Apple Notes' Format → More): for the edit menu.
enum ChecklistActions {
    static func menu(_ f: Formatting, run: @escaping (EditorCommand) -> Void) -> UIMenu {
        func item(_ title: String, _ symbol: String, _ c: EditorCommand, enabled: Bool, destructive: Bool = false) -> UIAction {
            var attributes: UIMenuElement.Attributes = []
            if !enabled { attributes.insert(.disabled) }
            if destructive { attributes.insert(.destructive) }
            return UIAction(title: title, image: UIImage(systemName: symbol), attributes: attributes) { _ in run(c) }
        }
        return UIMenu(title: "Checklist", image: UIImage(systemName: "checklist"), children: [
            item("Check All", "checkmark.circle", .checkAll, enabled: f.checklistHasUnchecked),
            item("Uncheck All", "circle", .uncheckAll, enabled: f.checklistHasChecked),
            item("Move Checked to Bottom", "arrow.down.to.line", .moveCheckedToBottom, enabled: f.checklistHasChecked),
            item("Delete Checked Items", "trash", .deleteChecked, enabled: f.checklistHasChecked, destructive: true),
        ])
    }
}

/// The Table menu (keyboard bar, edit menu): Insert Table, or in a table,
/// moving to the next cell (there's no Tab on the iPhone's keyboard), rows,
/// columns, headers and deleting.
enum TableActions {
    static func items(_ f: Formatting?, run: @escaping (EditorCommand) -> Void) -> [UIMenuElement] {
        func item(_ title: String, _ symbol: String, _ c: EditorCommand, on: Bool = false, destructive: Bool = false) -> UIAction {
            UIAction(title: title, image: UIImage(systemName: symbol), attributes: destructive ? [.destructive] : [], state: on ? .on : .off) { _ in run(c) }
        }
        guard let f, f.inTable else {
            return [item("Insert Table", "tablecells", .insertTable)]
        }
        return [
            UIMenu(options: .displayInline, children: [
                item("Next Cell", "arrow.right.to.line", .nextCell),
                item("Previous Cell", "arrow.left.to.line", .previousCell),
            ]),
            UIMenu(options: .displayInline, children: [
                item("Add Row Above", "arrow.up.to.line", .addRowAbove),
                item("Add Row Below", "arrow.down.to.line", .addRowBelow),
                item("Add Column Before", "arrow.left.to.line.compact", .addColumnBefore),
                item("Add Column After", "arrow.right.to.line.compact", .addColumnAfter),
            ]),
            UIMenu(options: .displayInline, children: [
                item("Header Row", "rectangle.topthird.inset.filled", .toggleHeaderRow, on: f.headerRow),
                item("Header Column", "rectangle.leftthird.inset.filled", .toggleHeaderColumn, on: f.headerColumn),
            ]),
            UIMenu(options: .displayInline, children: [
                item("Delete Row", "minus.square", .deleteRow, destructive: true),
                item("Delete Column", "minus.square", .deleteColumn, destructive: true),
                item("Delete Table", "trash", .deleteTable, destructive: true),
            ]),
        ]
    }
}

/// What the note screen and the format panel see of the editor.
final class EditorBridge: ObservableObject {
    @Published var formatting: Formatting?
    @Published var isEditing = false
    @Published var showingFormat = false
    weak var coordinator: NoteEditorView.Coordinator?

    func command(_ c: EditorCommand) { coordinator?.command(c) }
    func toggleFormatPanel() { coordinator?.toggleFormatPanel() }
    func endEditing() { coordinator?.textView?.resignFirstResponder() }
    func beginEditing() { coordinator?.textView?.becomeFirstResponder() }
    /// Find in Note; with `text`, its matches are found and shown at once
    /// (a note opened from search results).
    func find(_ text: String = "") {
        guard let tv = coordinator?.textView, let finder = tv.findInteraction else { return }
        if !text.isEmpty {
            finder.searchText = text
        }
        finder.presentFindNavigator(showingReplace: false)
    }
    func linkAtSelection() -> String { coordinator?.editor.linkAtSelection() ?? "" }

    /// Prints the note as it looks: list markers and checkboxes included.
    func printNote(title: String) {
        guard let c = coordinator else { return }
        let text = NoteTextRenderer.printable(c.editor.state().paragraphs, textSize: c.textSize)
        let info = UIPrintInfo(dictionary: nil)
        info.outputType = .general
        info.jobName = title
        let formatter = UISimpleTextPrintFormatter(attributedText: text)
        formatter.perPageContentInsets = UIEdgeInsets(top: 54, left: 54, bottom: 54, right: 54)
        let controller = UIPrintInteractionController.shared
        controller.printInfo = info
        controller.printFormatter = formatter
        controller.present(animated: true, completionHandler: nil)
    }
}

struct NoteEditorView: UIViewRepresentable {
    let editor: NoteEditor
    let store: NotesStore
    let bridge: EditorBridge
    let date: String
    let readOnly: Bool
    let startEditing: Bool
    /// Typing into a Recently Deleted note.
    let onBlockedEdit: () -> Void
    /// Asked to add a link.
    let onLink: () -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(editor: editor, store: store, bridge: bridge)
    }

    func makeUIView(context: Context) -> NoteTextView {
        let tv = NoteTextView()
        let c = context.coordinator
        c.readOnly = readOnly
        c.onBlockedEdit = onBlockedEdit
        c.onLink = onLink
        c.attach(tv)
        tv.dateLabel.text = date
        if startEditing && !readOnly {
            DispatchQueue.main.async { tv.becomeFirstResponder() }
        }
        return tv
    }

    func updateUIView(_ tv: NoteTextView, context: Context) {
        let c = context.coordinator
        c.onBlockedEdit = onBlockedEdit
        c.onLink = onLink
        tv.dateLabel.text = date
        if c.readOnly != readOnly {
            c.readOnly = readOnly
            tv.isEditable = !readOnly
        }
        let size = store.core.settings().textSize
        if size != c.textSize {
            c.textSize = size
            c.render()
        }
    }

    final class Coordinator: NSObject, UITextViewDelegate, UIGestureRecognizerDelegate, PHPickerViewControllerDelegate, UIDocumentPickerDelegate {
        let editor: NoteEditor
        let store: NotesStore
        let bridge: EditorBridge
        weak var textView: NoteTextView?
        var readOnly = false
        var textSize: Float
        var sortChecked: Bool
        var onBlockedEdit: () -> Void = {}
        var onLink: () -> Void = {}
        private(set) var formatting: Formatting?
        /// The attached file being previewed (Quick Look keeps its data
        /// source weakly).
        private var preview: AttachmentPreview?
        /// Setting the text view's text or selection ourselves.
        private var rendering = false
        private var formatHost: UIHostingController<AnyView>?

        init(editor: NoteEditor, store: NotesStore, bridge: EditorBridge) {
            self.editor = editor
            self.store = store
            self.bridge = bridge
            let s = store.core.settings()
            textSize = s.textSize
            sortChecked = s.sortChecked
        }

        /// Sets up the text view to show and edit the note (the app's, or a
        /// test's).
        func attach(_ tv: NoteTextView) {
            textView = tv
            tv.coordinator = self
            tv.engineUndo.coordinator = self
            tv.delegate = self
            tv.backgroundColor = .systemBackground
            tv.alwaysBounceVertical = true
            tv.keyboardDismissMode = .interactive
            tv.textContainerInset = UIEdgeInsets(top: 34, left: 16, bottom: 120, right: 16)
            tv.textContainer.lineFragmentPadding = 0
            tv.isFindInteractionEnabled = true
            tv.isEditable = !readOnly
            tv.dataDetectorTypes = []
            tv.linkTextAttributes = [.foregroundColor: UIColor.notesAccent, .underlineStyle: NSUnderlineStyle.single.rawValue]
            tv.autocapitalizationType = .sentences
            tv.smartDashesType = .no
            tv.smartQuotesType = .yes
            tv.inputAccessoryView = makeAccessoryBar()
            let tap = UITapGestureRecognizer(target: self, action: #selector(tapped(_:)))
            tap.delegate = self
            tv.addGestureRecognizer(tap)
            bridge.coordinator = self
            render()
        }

        // ---- Showing the note ----

        /// Shows the whole note and the engine's selection.
        func render() {
            guard let tv = textView else { return }
            let state = editor.state()
            rendering = true
            tv.notesLayout.textSize = textSize
            let text = NoteTextRenderer.attributed(state.paragraphs, textSize: textSize, images: imageLayout())
            tv.storage.beginEditing()
            tv.storage.setAttributedString(text)
            tv.storage.endEditing()
            updateTrailingMark(state.paragraphs)
            let start = Int(state.selectionStart)
            let end = Int(state.selectionEnd)
            tv.selectedRange = NSRange(location: min(start, text.length), length: max(0, min(end, text.length) - min(start, text.length)))
            rendering = false
            refreshFormatting()
            tv.setNeedsDisplay()
        }

        /// How wide pictures may be: the text's width.
        private func imageLayout() -> ImageLayout {
            guard let tv = textView else { return .standard }
            let w = tv.bounds.width - tv.textContainerInset.left - tv.textContainerInset.right - 2 * tv.textContainer.lineFragmentPadding - 2
            return ImageLayout(width: w > 0 ? w : ImageLayout.standard.width, scale: tv.traitCollection.displayScale)
        }

        /// The text view's width changed: pictures are fitted again.
        func widthChanged() {
            if rendering { return }
            // (A large PDF's page is as wide as the text too.)
            if editor.state().paragraphs.contains(where: { p in p.file != nil || p.runs.contains { $0.image != nil } }) {
                render()
            }
        }

        // ---- Pictures ----

        /// Puts a picture (an image file's bytes) at the caret.
        func attachImage(_ data: Data) {
            if readOnly {
                blockedEdit()
                return
            }
            // A locked note's pictures are encrypted (the core does it).
            guard let name = try? store.core.addNoteImage(id: editor.noteId(), data: NoteImages.portable(data)) else {
                store.show("That picture can't be added.")
                return
            }
            command(.insertImage(name: name))
        }

        @objc fileprivate func photoTapped() {
            if readOnly {
                blockedEdit()
                return
            }
            var config = PHPickerConfiguration()
            config.filter = .images
            config.selectionLimit = 0
            let picker = PHPickerViewController(configuration: config)
            picker.delegate = self
            guard var top = textView?.window?.rootViewController else { return }
            while let next = top.presentedViewController { top = next }
            top.present(picker, animated: true)
        }

        // ---- Other files (PDFs, …) ----

        /// The view controller to present over.
        private func topController() -> UIViewController? {
            guard var top = textView?.window?.rootViewController else { return nil }
            while let next = top.presentedViewController { top = next }
            return top
        }

        fileprivate func fileTapped() {
            if readOnly {
                blockedEdit()
                return
            }
            let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.item], asCopy: true)
            picker.allowsMultipleSelection = true
            picker.delegate = self
            topController()?.present(picker, animated: true)
        }

        func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
            for url in urls {
                attachFile(url)
            }
        }

        /// Puts a file at the caret: a picture shown, another file as a
        /// link with its name.
        func attachFile(_ url: URL) {
            if readOnly {
                blockedEdit()
                return
            }
            let label = url.lastPathComponent
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            guard let data = try? Data(contentsOf: url) else {
                store.show("\(label) can't be read.")
                return
            }
            if let type = UTType(filenameExtension: url.pathExtension), type.conforms(to: .image), UIImage(data: data) != nil {
                attachImage(data)
                return
            }
            // A locked note's files are encrypted (the core does it).
            do {
                let reference = try store.core.addNoteFile(id: editor.noteId(), data: data, fileName: label)
                command(.insertFile(reference: reference, label: label))
            } catch {
                store.show("\(label) can't be attached.")
            }
        }

        /// Opens an attached file (its link tapped) in Quick Look, from a
        /// copy named as it's shown.
        func openAttachment(reference: String, label: String) {
            guard let path = try? store.core.openAttachment(reference: reference, label: label) else {
                store.show("\(label) isn't on this device yet.")
                return
            }
            let source = AttachmentPreview(url: URL(fileURLWithPath: path))
            preview = source
            let ql = QLPreviewController()
            ql.dataSource = source
            topController()?.present(ql, animated: true)
        }

        func picker(_ picker: PHPickerViewController, didFinishPicking results: [PHPickerResult]) {
            picker.dismiss(animated: true)
            for r in results where r.itemProvider.hasItemConformingToTypeIdentifier(UTType.image.identifier) {
                r.itemProvider.loadDataRepresentation(forTypeIdentifier: UTType.image.identifier) { [weak self] data, _ in
                    guard let data else { return }
                    DispatchQueue.main.async { self?.attachImage(data) }
                }
            }
        }

        private func updateTrailingMark(_ paragraphs: [Paragraph]) {
            guard let tv = textView else { return }
            if let last = paragraphs.last, last.runs.isEmpty {
                tv.notesLayout.trailingMark = ParagraphMark(last)
            } else {
                tv.notesLayout.trailingMark = nil
            }
        }

        /// After plain typing: the paragraph at the caret is styled again
        /// (links and tags as typed, and one style throughout).
        private func restyleCurrentParagraph() {
            guard let tv = textView else { return }
            let index = editor.paragraphAt(offset: UInt32(tv.selectedRange.location))
            guard let p = editor.paragraph(index: index) else { return }
            let start = Int(editor.paragraphStart(index: index))
            let last = index + 1 >= editor.paragraphCount()
            let styled = NoteTextRenderer.attributed(p, textSize: textSize, terminated: !last, images: imageLayout())
            let range = NSRange(location: start, length: styled.length)
            guard NSMaxRange(range) <= tv.storage.length,
                  (tv.storage.string as NSString).substring(with: range) == styled.string else {
                render()
                return
            }
            rendering = true
            let selection = tv.selectedRange
            tv.storage.beginEditing()
            styled.enumerateAttributes(in: NSRange(location: 0, length: styled.length)) { attrs, r, _ in
                tv.storage.setAttributes(attrs, range: NSRange(location: start + r.location, length: r.length))
            }
            tv.storage.endEditing()
            tv.selectedRange = selection
            rendering = false
            if last {
                tv.notesLayout.trailingMark = p.runs.isEmpty ? ParagraphMark(p) : nil
            }
            tv.setNeedsDisplay()
        }

        /// The formatting at the caret: for typing, the Aa panel and undo.
        func refreshFormatting() {
            guard let tv = textView else { return }
            let f = editor.formatting()
            formatting = f
            // Not during SwiftUI's own update of the view (render runs in
            // makeUIView and updateUIView).
            DispatchQueue.main.async { [bridge] in bridge.formatting = f }
            let index = editor.paragraphAt(offset: UInt32(tv.selectedRange.location))
            if let p = editor.paragraph(index: index) {
                tv.typingAttributes = NoteTextRenderer.typingAttributes(p, formatting: f, textSize: textSize)
            }
        }

        /// The note changed: saved soon, and the lists redraw.
        func edited() {
            store.core.touchLock()
            saveSoon()
        }

        private var saveTask: Task<Void, Never>?
        private func saveSoon() {
            saveTask?.cancel()
            saveTask = Task { @MainActor [weak self] in
                try? await Task.sleep(for: .milliseconds(600))
                guard let self, !Task.isCancelled else { return }
                if !self.store.core.saveEditor(editor: self.editor) {
                    self.store.changed()
                }
            }
        }

        // ---- Commands ----

        func command(_ c: EditorCommand) {
            if readOnly {
                blockedEdit()
                return
            }
            editor.command(command: c)
            render()
            edited()
        }

        func blockedEdit() {
            onBlockedEdit()
        }

        // ---- UITextViewDelegate ----

        func textView(_ tv: UITextView, shouldChangeTextIn range: NSRange, replacementText text: String) -> Bool {
            if rendering { return true }
            if readOnly {
                blockedEdit()
                return false
            }
            let native = editor.replace(start: UInt32(range.location), end: UInt32(NSMaxRange(range)), text: text)
            if native {
                return true
            }
            render()
            edited()
            return false
        }

        func textViewDidChange(_ tv: UITextView) {
            if rendering || tv.markedTextRange != nil { return }
            // The text view changed something without saying how (some
            // keyboards do): the engine catches up.
            if editor.syncText(text: tv.text) {
                render()
            } else {
                restyleCurrentParagraph()
                refreshFormatting()
            }
            edited()
        }

        func textViewDidChangeSelection(_ tv: UITextView) {
            if rendering { return }
            let r = tv.selectedRange
            editor.select(start: UInt32(r.location), end: UInt32(NSMaxRange(r)))
            refreshFormatting()
        }

        /// The edit menu (touch and hold, or a selection): Format (B I U S),
        /// Add Link, and for list items Move Up and Down and the checklist's
        /// actions, after the system's own.
        func textView(_ tv: UITextView, editMenuForTextIn range: NSRange, suggestedActions: [UIMenuElement]) -> UIMenu? {
            if readOnly { return nil }
            let f = formatting
            let action = { (title: String, symbol: String, on: Bool, c: EditorCommand) -> UIAction in
                UIAction(title: title, image: UIImage(systemName: symbol), state: on ? .on : .off) { [weak self] _ in
                    self?.command(c)
                }
            }
            let format = UIMenu(title: "Format", image: UIImage(systemName: "textformat"), children: [
                action("Bold", "bold", f?.bold == true, .bold),
                action("Italic", "italic", f?.italic == true, .italic),
                action("Underline", "underline", f?.underline == true, .underline),
                action("Strikethrough", "strikethrough", f?.strike == true, .strikethrough),
            ])
            let link = UIAction(title: (f?.link ?? "").isEmpty ? "Add Link" : "Edit Link", image: UIImage(systemName: "link")) { [weak self] _ in
                self?.onLink()
            }
            var mine: [UIMenuElement] = [format, link]
            if f?.isList == true {
                mine.append(action("Move Up", "arrow.up", false, .moveUp))
                mine.append(action("Move Down", "arrow.down", false, .moveDown))
            }
            if let f, f.style == .checklist {
                mine.append(ChecklistActions.menu(f) { [weak self] c in self?.command(c) })
            }
            if let f, f.inTable {
                mine.append(UIMenu(title: "Table", image: UIImage(systemName: "tablecells"), children: TableActions.items(f) { [weak self] c in self?.command(c) }))
            }
            return UIMenu(children: suggestedActions + mine)
        }

        func textViewDidBeginEditing(_ tv: UITextView) {
            bridge.isEditing = true
        }

        func textViewDidEndEditing(_ tv: UITextView) {
            bridge.isEditing = false
            if bridge.showingFormat {
                bridge.showingFormat = false
                tv.inputView = nil
            }
            _ = store.core.saveEditor(editor: editor)
            store.changed()
        }

        // ---- Checklist circles and links ----

        /// What a tap at a point hits: a checklist circle (its paragraph),
        /// or a link.
        /// What's at a point: a checklist circle (its paragraph), a link, or
        /// an attached file's card outside its name (the file, and the name
        /// it's shown with).
        private func hit(_ point: CGPoint) -> (checkbox: UInt32?, link: URL?, file: (reference: String, label: String)?) {
            let none: (checkbox: UInt32?, link: URL?, file: (reference: String, label: String)?) = (nil, nil, nil)
            guard let tv = textView, tv.storage.length > 0 else { return none }
            let p = CGPoint(x: point.x - tv.textContainerInset.left, y: point.y - tv.textContainerInset.top)
            let lm = tv.notesLayout
            let glyph = lm.glyphIndex(for: p, in: tv.textContainer)
            let line = lm.lineFragmentRect(forGlyphAt: glyph, effectiveRange: nil)
            guard p.y >= line.minY - 4, p.y <= line.maxY + 4 else { return none }
            let char = lm.characterIndexForGlyph(at: glyph)
            guard char < tv.storage.length else { return none }
            if let mark = tv.storage.attribute(.cnParagraph, at: char, effectiveRange: nil) as? ParagraphMark, mark.style == .checklist {
                let body = NoteTextRenderer.bodySize(textSize)
                let x0 = CGFloat(mark.indent) * NoteTextRenderer.indentStep(body)
                if p.x >= x0 - 8, p.x <= x0 + NoteTextRenderer.markerWidth(.checklist, body: body) {
                    let text = tv.storage.string as NSString
                    let start = text.paragraphRange(for: NSRange(location: char, length: 0)).location
                    return (editor.paragraphAt(offset: UInt32(start)), nil, nil)
                }
            }
            let glyphRect = lm.boundingRect(forGlyphRange: NSRange(location: glyph, length: 1), in: tv.textContainer)
            if glyphRect.insetBy(dx: -2, dy: -2).contains(p), let url = tv.storage.attribute(.link, at: char, effectiveRange: nil) as? URL {
                return (nil, url, nil)
            }
            if let mark = tv.storage.attribute(.cnParagraph, at: char, effectiveRange: nil) as? ParagraphMark, let file = mark.file {
                let text = tv.storage.string as NSString
                let label = text.substring(with: text.paragraphRange(for: NSRange(location: char, length: 0))).trimmingCharacters(in: .newlines)
                return (nil, nil, (file.reference, label))
            }
            return none
        }

        /// The whole text of the link at a point (an attached file's name).
        private func linkText(at point: CGPoint) -> String {
            guard let tv = textView, tv.storage.length > 0 else { return "" }
            let p = CGPoint(x: point.x - tv.textContainerInset.left, y: point.y - tv.textContainerInset.top)
            let lm = tv.notesLayout
            let char = lm.characterIndexForGlyph(at: lm.glyphIndex(for: p, in: tv.textContainer))
            guard char < tv.storage.length else { return "" }
            var range = NSRange(location: 0, length: 0)
            guard tv.storage.attribute(.link, at: char, longestEffectiveRange: &range, in: NSRange(location: 0, length: tv.storage.length)) != nil else { return "" }
            return (tv.storage.string as NSString).substring(with: range)
        }

        func gestureRecognizerShouldBegin(_ g: UIGestureRecognizer) -> Bool {
            guard let tv = textView else { return false }
            let h = hit(g.location(in: tv))
            if h.checkbox != nil || h.file != nil { return true }
            // Links open with a tap while not typing (typing, a tap puts
            // the caret in them).
            if h.link != nil && !tv.isFirstResponder { return true }
            return false
        }

        func gestureRecognizer(_ g: UIGestureRecognizer, shouldBeRequiredToFailBy other: UIGestureRecognizer) -> Bool {
            true
        }

        @objc func tapped(_ g: UITapGestureRecognizer) {
            guard let tv = textView else { return }
            let h = hit(g.location(in: tv))
            if let index = h.checkbox {
                if readOnly {
                    blockedEdit()
                    return
                }
                UIImpactFeedbackGenerator(style: .light).impactOccurred()
                editor.toggleCheck(paragraph: index, sortChecked: store.core.settings().sortChecked)
                render()
                edited()
            } else if let file = h.file {
                openAttachment(reference: file.reference, label: file.label)
            } else if let url = h.link {
                let address = url.absoluteString
                if url.scheme == nil, address.hasPrefix("attachments/") {
                    // An attached file: its name is the link's text.
                    openAttachment(reference: String(address.dropFirst("attachments/".count)), label: linkText(at: g.location(in: tv)))
                } else {
                    UIApplication.shared.open(url)
                }
            }
        }

        // ---- The keyboard bar and the Aa panel ----

        func makeAccessoryBar() -> UIView {
            let bar = UIToolbar(frame: CGRect(x: 0, y: 0, width: 320, height: 44))
            let item = { (symbol: String, label: String, action: Selector) -> UIBarButtonItem in
                let b = UIBarButtonItem(image: UIImage(systemName: symbol), style: .plain, target: self, action: action)
                b.accessibilityLabel = label
                return b
            }
            let flex = { UIBarButtonItem(barButtonSystemItem: .flexibleSpace, target: nil, action: nil) }
            // The Table button's menu is made when it opens: Insert Table,
            // or in a table its rows, columns and headers.
            let table = UIBarButtonItem(image: UIImage(systemName: "tablecells"), menu: UIMenu(children: [
                UIDeferredMenuElement.uncached { [weak self] completion in
                    completion(TableActions.items(self?.formatting) { c in self?.command(c) })
                },
            ]))
            table.accessibilityLabel = "Table"
            let attach = UIBarButtonItem(image: UIImage(systemName: "paperclip"), menu: UIMenu(children: [
                UIAction(title: "Choose Photo", image: UIImage(systemName: "photo.on.rectangle")) { [weak self] _ in self?.photoTapped() },
                UIAction(title: "Attach File", image: UIImage(systemName: "doc")) { [weak self] _ in self?.fileTapped() },
            ]))
            attach.accessibilityLabel = "Attach"
            bar.items = [
                table, flex(),
                item("textformat", "Format", #selector(formatTapped)), flex(),
                item("checklist", "Checklist", #selector(checklistTapped)), flex(),
                item("decrease.indent", "Outdent", #selector(outdentTapped)), flex(),
                item("increase.indent", "Indent", #selector(indentTapped)), flex(),
                attach, flex(),
                item("link", "Link", #selector(linkTapped)), flex(),
                item("xmark.circle", "Done", #selector(doneTapped)),
            ]
            bar.tintColor = .label
            return bar
        }

        @objc private func formatTapped() { toggleFormatPanel() }
        @objc private func checklistTapped() { command(.toggleChecklist) }
        @objc private func indentTapped() { command(.indent) }
        @objc private func outdentTapped() { command(.outdent) }
        @objc private func linkTapped() { onLink() }
        @objc private func doneTapped() { textView?.resignFirstResponder() }

        /// The Aa panel takes the keyboard's place, as in Apple Notes.
        func toggleFormatPanel() {
            guard let tv = textView else { return }
            if readOnly {
                blockedEdit()
                return
            }
            if bridge.showingFormat {
                bridge.showingFormat = false
                tv.inputView = nil
            } else {
                let host = formatHost ?? UIHostingController(rootView: AnyView(FormatPanel(bridge: bridge)))
                formatHost = host
                host.view.frame = CGRect(x: 0, y: 0, width: tv.bounds.width, height: 300)
                host.view.autoresizingMask = [.flexibleWidth]
                host.view.backgroundColor = .secondarySystemBackground
                tv.inputView = host.view
                bridge.showingFormat = true
                if !tv.isFirstResponder { tv.becomeFirstResponder() }
            }
            tv.reloadInputViews()
        }
    }
}

/// Attributed text in SwiftUI (for previews).
struct AttributedTextPreview: UIViewRepresentable {
    let text: NSAttributedString

    func makeUIView(context: Context) -> UITextView {
        let tv = UITextView()
        tv.isEditable = false
        tv.isScrollEnabled = false
        tv.textContainerInset = UIEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        tv.attributedText = text
        tv.backgroundColor = .systemBackground
        return tv
    }

    func updateUIView(_ tv: UITextView, context: Context) {
        tv.attributedText = text
    }
}

/// One attached file for Quick Look to show.
final class AttachmentPreview: NSObject, QLPreviewControllerDataSource {
    let url: URL

    init(url: URL) {
        self.url = url
    }

    func numberOfPreviewItems(in controller: QLPreviewController) -> Int { 1 }

    func previewController(_ controller: QLPreviewController, previewItemAt index: Int) -> QLPreviewItem {
        url as NSURL
    }
}
