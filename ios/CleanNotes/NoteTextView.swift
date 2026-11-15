import SwiftUI
import UIKit

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
    let number: Int

    init(_ p: Paragraph) {
        style = p.style
        indent = Int(p.indent)
        checked = p.checked
        number = Int(p.number)
    }

    var isList: Bool {
        switch style {
        case .bulleted, .dashed, .numbered, .checklist: return true
        default: return false
        }
    }
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

    static func attributed(_ paragraphs: [Paragraph], textSize: Float) -> NSAttributedString {
        let out = NSMutableAttributedString()
        for (i, p) in paragraphs.enumerated() {
            out.append(attributed(p, textSize: textSize, terminated: i + 1 < paragraphs.count))
        }
        return out
    }

    /// One paragraph, with its line break if another follows.
    static func attributed(_ p: Paragraph, textSize: Float, terminated: Bool) -> NSAttributedString {
        let out = NSMutableAttributedString()
        let mark = ParagraphMark(p)
        let para = paragraphStyle(p, textSize: textSize)
        for run in p.runs {
            out.append(NSAttributedString(string: run.text, attributes: attributes(p, run: run, textSize: textSize, mark: mark, para: para)))
        }
        if terminated {
            out.append(NSAttributedString(string: "\n", attributes: attributes(p, run: nil, textSize: textSize, mark: mark, para: para)))
        }
        return out
    }

    static func paragraphStyle(_ p: Paragraph, textSize: Float) -> NSParagraphStyle {
        let body = bodySize(textSize)
        let s = NSMutableParagraphStyle()
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

    /// What typing at the caret should look like.
    static func typingAttributes(_ p: Paragraph, formatting f: Formatting, textSize: Float) -> [NSAttributedString.Key: Any] {
        let run = TextRun(text: "", bold: f.bold, italic: f.italic, underline: f.underline, strike: f.strike, code: f.code, link: f.link, tag: false)
        return attributes(p, run: run, textSize: textSize)
    }
}

/// Draws list markers, checklist circles, quote bars and code block
/// backgrounds next to the text.
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
            let label: String
            switch mark.style {
            case .bulleted: label = "•"
            case .dashed: label = "–"
            default: label = "\(mark.number)."
            }
            let attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: UIColor.label]
            let s = NSAttributedString(string: label, attributes: attrs)
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

    override func layoutSubviews() {
        super.layoutSubviews()
        dateLabel.frame = CGRect(x: 0, y: 6, width: bounds.width, height: 18)
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
        guard let text = UIPasteboard.general.string else { return }
        c.editor.paste(text: text)
        c.render()
        c.edited()
    }

    override func toggleBoldface(_ sender: Any?) { coordinator?.command(.bold) }
    override func toggleItalics(_ sender: Any?) { coordinator?.command(.italic) }
    override func toggleUnderline(_ sender: Any?) { coordinator?.command(.underline) }
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
    func find() {
        guard let tv = coordinator?.textView else { return }
        tv.findInteraction?.presentFindNavigator(showingReplace: false)
    }
    func linkAtSelection() -> String { coordinator?.editor.linkAtSelection() ?? "" }
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
        c.textView = tv
        c.readOnly = readOnly
        c.onBlockedEdit = onBlockedEdit
        c.onLink = onLink
        tv.coordinator = c
        tv.engineUndo.coordinator = c
        tv.delegate = c
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
        tv.dateLabel.text = date
        tv.inputAccessoryView = c.makeAccessoryBar()
        let tap = UITapGestureRecognizer(target: c, action: #selector(Coordinator.tapped(_:)))
        tap.delegate = c
        tv.addGestureRecognizer(tap)
        bridge.coordinator = c
        c.render()
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

    final class Coordinator: NSObject, UITextViewDelegate, UIGestureRecognizerDelegate {
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

        // ---- Showing the note ----

        /// Shows the whole note and the engine's selection.
        func render() {
            guard let tv = textView else { return }
            let state = editor.state()
            rendering = true
            tv.notesLayout.textSize = textSize
            let text = NoteTextRenderer.attributed(state.paragraphs, textSize: textSize)
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
            let styled = NoteTextRenderer.attributed(p, textSize: textSize, terminated: !last)
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
        private func hit(_ point: CGPoint) -> (checkbox: UInt32?, link: URL?) {
            guard let tv = textView, tv.storage.length > 0 else { return (nil, nil) }
            let p = CGPoint(x: point.x - tv.textContainerInset.left, y: point.y - tv.textContainerInset.top)
            let lm = tv.notesLayout
            let glyph = lm.glyphIndex(for: p, in: tv.textContainer)
            let line = lm.lineFragmentRect(forGlyphAt: glyph, effectiveRange: nil)
            guard p.y >= line.minY - 4, p.y <= line.maxY + 4 else { return (nil, nil) }
            let char = lm.characterIndexForGlyph(at: glyph)
            guard char < tv.storage.length else { return (nil, nil) }
            if let mark = tv.storage.attribute(.cnParagraph, at: char, effectiveRange: nil) as? ParagraphMark, mark.style == .checklist {
                let body = NoteTextRenderer.bodySize(textSize)
                let x0 = CGFloat(mark.indent) * NoteTextRenderer.indentStep(body)
                if p.x >= x0 - 8, p.x <= x0 + NoteTextRenderer.markerWidth(.checklist, body: body) {
                    let text = tv.storage.string as NSString
                    let start = text.paragraphRange(for: NSRange(location: char, length: 0)).location
                    return (editor.paragraphAt(offset: UInt32(start)), nil)
                }
            }
            let glyphRect = lm.boundingRect(forGlyphRange: NSRange(location: glyph, length: 1), in: tv.textContainer)
            if glyphRect.insetBy(dx: -2, dy: -2).contains(p), let url = tv.storage.attribute(.link, at: char, effectiveRange: nil) as? URL {
                return (nil, url)
            }
            return (nil, nil)
        }

        func gestureRecognizerShouldBegin(_ g: UIGestureRecognizer) -> Bool {
            guard let tv = textView else { return false }
            let h = hit(g.location(in: tv))
            if h.checkbox != nil { return true }
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
            } else if let url = h.link {
                UIApplication.shared.open(url)
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
            bar.items = [
                item("textformat", "Format", #selector(formatTapped)), flex(),
                item("checklist", "Checklist", #selector(checklistTapped)), flex(),
                item("decrease.indent", "Outdent", #selector(outdentTapped)), flex(),
                item("increase.indent", "Indent", #selector(indentTapped)), flex(),
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
