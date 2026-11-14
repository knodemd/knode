import AppKit

extension NSAttributedString.Key {
    /// The name a `[[wiki link]]` of the source points to, for opening it with Command-click.
    static let knodeWikiLink = NSAttributedString.Key("md.knode.wikiLink")
    /// The `MarkdownLineState` after the line, as its `code`, on every character of the line.
    static let knodeLineState = NSAttributedString.Key("md.knode.lineState")
    /// Marks the underlines the highlighter drew, so it only ever removes its own.
    static let knodeUnderline = NSAttributedString.Key("md.knode.underline")
}

/// Colours the Markdown source in a text storage, as its delegate.
///
/// Each edit restyles only the lines it touched. Every line keeps the state it ends in
/// (`knodeLineState`), so the next line knows whether it starts in a code block or the
/// frontmatter; when an edit changes that state, the lines after it are restyled until the
/// state agrees with what it was before. Only attributes change, never characters, so the
/// undo stack, the selection and the text sent to co-editors stay as they were. While an input
/// method composes text, colouring waits until the text is committed.
@MainActor
final class MarkdownHighlighter: NSObject, NSTextStorageDelegate {
    /// Whether an input method is composing text now; styling waits until it is done.
    var isComposing: () -> Bool = { false }
    /// The range edited while text was composed, restyled when composing ends.
    private var pending: NSRange?

    nonisolated func textStorage(_ textStorage: NSTextStorage, willProcessEditing editedMask: NSTextStorageEditActions, range editedRange: NSRange, changeInLength delta: Int) {
        guard editedMask.contains(.editedCharacters) else { return }
        // Text is only edited on the main thread, where the text view lives.
        nonisolated(unsafe) let textStorage = textStorage
        MainActor.assumeIsolated {
            if isComposing() {
                pending = pending.map { NSUnionRange($0, editedRange) } ?? editedRange
                return
            }
            restyle(textStorage, editedRange: editedRange)
        }
    }

    /// Restyles what was edited while text was composed. Call when composing ends.
    func finishComposing(_ storage: NSTextStorage) {
        guard let range = pending, !isComposing() else { return }
        pending = nil
        let length = storage.length
        let location = min(range.location, length)
        let clamped = NSRange(location: location, length: min(NSMaxRange(range), length) - location)
        storage.beginEditing()
        restyle(storage, editedRange: clamped)
        storage.endEditing()
    }

    /// Restyles the lines of `editedRange`, and the lines after them whose start changed.
    func restyle(_ storage: NSTextStorage, editedRange: NSRange) {
        let string = storage.string as NSString
        let length = string.length
        var location = string.paragraphRange(for: NSRange(location: min(editedRange.location, length), length: 0)).location
        var state = MarkdownLineState.normal
        if location > 0 {
            if let before = Self.state(in: storage, at: location - 1) {
                state = before
            } else {
                // Never styled before this line: start from the top.
                location = 0
            }
        }
        let editedEnd = min(NSMaxRange(editedRange), length)

        repeat {
            var start = 0, end = 0, contentsEnd = 0
            string.getParagraphStart(&start, end: &end, contentsEnd: &contentsEnd, for: NSRange(location: location, length: 0))
            let previous = end > start ? Self.state(in: storage, at: end - 1) : nil
            let line = NSRange(location: start, length: end - start)
            let content = string.substring(with: NSRange(location: start, length: contentsEnd - start)) as NSString
            let (tokens, next) = MarkdownSyntax.tokenize(content, from: state, isFirstLine: start == 0)
            if line.length > 0 {
                Self.apply(tokens, to: storage, line: line)
                storage.addAttribute(.knodeLineState, value: next.code, range: line)
            }
            state = next
            location = end
            // Past the edit, a line that ends as it did before leaves the rest as it was.
            if end >= editedEnd, previous == next { break }
        } while location < length
    }

    private static func state(in storage: NSTextStorage, at index: Int) -> MarkdownLineState? {
        (storage.attribute(.knodeLineState, at: index, effectiveRange: nil) as? Int).flatMap(MarkdownLineState.init(code:))
    }

    // MARK: Styles

    /// Resets the line to plain source, then applies each token over it.
    static func apply(_ tokens: [MarkdownSyntax.Token], to storage: NSTextStorage, line: NSRange) {
        storage.addAttributes(MarkdownStyle.base, range: line)
        for key: NSAttributedString.Key in [.backgroundColor, .strikethroughStyle, .strikethroughColor, .obliqueness, .knodeWikiLink] {
            storage.removeAttribute(key, range: line)
        }
        // Only the underlines drawn here: an input method's underline stays.
        var underlined: [NSRange] = []
        storage.enumerateAttribute(.knodeUnderline, in: line, options: []) { value, range, _ in
            if value != nil { underlined.append(range) }
        }
        for range in underlined {
            for key: NSAttributedString.Key in [.underlineStyle, .underlineColor, .knodeUnderline] {
                storage.removeAttribute(key, range: range)
            }
        }

        var heading: Int?
        for token in tokens {
            let range = NSRange(location: line.location + token.range.location, length: token.range.length)
            guard range.length > 0, NSMaxRange(range) <= NSMaxRange(line) else { continue }
            switch token.kind {
            case .heading(let level):
                heading = level
                storage.addAttribute(.font, value: MarkdownStyle.heading(level), range: range)
            case .marker, .listMarker, .frontmatter, .rule:
                storage.addAttribute(.foregroundColor, value: MarkdownStyle.muted, range: range)
            case .bold:
                if heading == nil { storage.addAttribute(.font, value: MarkdownStyle.bold, range: range) }
            case .italic:
                storage.addAttribute(.obliqueness, value: MarkdownStyle.obliqueness, range: range)
            case .strikethrough:
                storage.addAttributes([.strikethroughStyle: NSUnderlineStyle.single.rawValue, .strikethroughColor: MarkdownStyle.muted], range: range)
            case .code, .codeBlock:
                storage.addAttribute(.backgroundColor, value: MarkdownStyle.panel, range: range)
            case .link, .tag:
                storage.addAttribute(.foregroundColor, value: MarkdownStyle.link, range: range)
            case .wikiLink(let name):
                storage.addAttributes([
                    .foregroundColor: MarkdownStyle.link,
                    .underlineStyle: NSUnderlineStyle.single.rawValue,
                    .underlineColor: MarkdownStyle.linkUnderline,
                    .knodeUnderline: true,
                    .knodeWikiLink: name,
                ], range: range)
            case .checkbox(let checked):
                storage.addAttribute(.foregroundColor, value: checked ? MarkdownStyle.link : MarkdownStyle.muted, range: range)
            case .done:
                storage.addAttributes([
                    .foregroundColor: MarkdownStyle.muted,
                    .strikethroughStyle: NSUnderlineStyle.single.rawValue,
                    .strikethroughColor: MarkdownStyle.muted,
                ], range: range)
            case .quote:
                storage.addAttributes([.foregroundColor: MarkdownStyle.muted, .obliqueness: MarkdownStyle.obliqueness], range: range)
            }
        }
    }
}

/// The type and colours of the source, all from the Knode tokens. The colours follow the
/// appearance by themselves, so switching between dark and light needs no restyling.
@MainActor
enum MarkdownStyle {
    static let size: CGFloat = 13.5
    static let font = KFont.nsMono(size)
    static let bold = KFont.nsMono(size, .bold)
    static let obliqueness: CGFloat = 0.16

    static let fg = KColor.ns(\.fg)
    static let muted = KColor.ns(\.muted)
    static let panel = KColor.ns(\.panel)
    /// Links and tags: indigo on ink; on paper ink, as indigo is never small text there.
    static let link = NSColor(name: nil) { appearance in
        NSColor(hex: isDark(appearance) ? KPalette.indigo : KPalette.light.fg)
    }
    /// Wiki links on paper carry an indigo underline, like the preview; on ink, none.
    static let linkUnderline = NSColor(name: nil) { appearance in
        isDark(appearance) ? .clear : NSColor(hex: KPalette.indigo)
    }

    /// Lines a little apart, like the web's code blocks.
    static let paragraphStyle: NSParagraphStyle = {
        let style = NSMutableParagraphStyle()
        style.lineSpacing = 5
        return style
    }()

    static let base: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: fg, .paragraphStyle: paragraphStyle]

    private static let headings: [NSFont] = [
        KFont.nsMono(22, .bold), KFont.nsMono(19, .bold), KFont.nsMono(16.5, .bold),
        KFont.nsMono(15, .semibold), KFont.nsMono(size, .semibold), KFont.nsMono(size, .semibold),
    ]

    /// Headings bigger and bolder by level, still in Geist Mono.
    static func heading(_ level: Int) -> NSFont { headings[max(1, min(level, 6)) - 1] }

    nonisolated private static func isDark(_ appearance: NSAppearance) -> Bool {
        appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
    }
}
