import Foundation

/// The Markdown syntax of one line of source, for colouring the editor.
///
/// Lines are read one at a time, carrying a `MarkdownLineState` from one line to the next, so
/// only the lines an edit touched need to be read again: the state after a line tells where the
/// next one starts (in a fenced code block, in the frontmatter, or in ordinary text). This is a
/// highlighter, not a parser: it follows the CommonMark rules closely enough for colour, and the
/// core (`knode-md`) stays the one parser of what a note means.
enum MarkdownSyntax {
    /// What a range of a line is.
    enum Kind: Equatable, Hashable, Sendable {
        /// The text of a heading, of level 1 to 6, including its markers.
        case heading(Int)
        /// Syntax shown in the muted colour: `#`, `**`, `_`, backticks, brackets, link targets.
        case marker
        case bold
        case italic
        case strikethrough
        /// The text of an inline code span, without its backticks.
        case code
        /// A whole line inside a fenced code block, or one of its fences (which are muted too).
        case codeBlock
        /// The text of a link, `[text](url)`, or a bare URL.
        case link
        /// The name inside `[[name]]` or `![[name]]`, without its alias.
        case wikiLink(String)
        case listMarker
        case checkbox(checked: Bool)
        /// The text of a checked item, `- [x] done`.
        case done
        /// The text of a block quote, after its `>`.
        case quote
        case tag
        /// A whole line of the YAML frontmatter at the top of the note.
        case frontmatter
        /// A thematic break, `---`.
        case rule
    }

    struct Token: Equatable, Sendable {
        var range: NSRange
        var kind: Kind

        init(_ location: Int, _ length: Int, _ kind: Kind) {
            range = NSRange(location: location, length: length)
            self.kind = kind
        }

        init(_ range: NSRange, _ kind: Kind) {
            self.range = range
            self.kind = kind
        }
    }

    /// Reads one line, without its line break, starting in `state`. Ranges are in UTF-16 units of
    /// `line`, in the order they are to be applied (later tokens go over earlier ones).
    static func tokenize(_ line: String, from state: MarkdownLineState, isFirstLine: Bool = false) -> (tokens: [Token], state: MarkdownLineState) {
        tokenize(line as NSString, from: state, isFirstLine: isFirstLine)
    }

    static func tokenize(_ line: NSString, from state: MarkdownLineState, isFirstLine: Bool = false) -> (tokens: [Token], state: MarkdownLineState) {
        let whole = NSRange(location: 0, length: line.length)
        let trimmed = line.trimmingCharacters(in: .whitespaces)

        switch state {
        case .frontmatter:
            let closes = trimmed == "---" || trimmed == "..."
            return ([Token(whole, .frontmatter)], closes ? .normal : .frontmatter)
        case .fence(let character, let count):
            if let fence = fence(line), fence.character == character, fence.count >= count, fence.info.isEmpty {
                return ([Token(whole, .codeBlock), Token(whole, .marker)], .normal)
            }
            return ([Token(whole, .codeBlock)], state)
        case .normal:
            break
        }

        if isFirstLine, trimmed == "---", line.hasPrefix("---") {
            return ([Token(whole, .frontmatter)], .frontmatter)
        }
        if let fence = fence(line), !(fence.character == "`" && fence.info.contains("`")) {
            return ([Token(whole, .codeBlock), Token(whole, .marker)], .fence(fence.character, fence.count))
        }
        if match(rule, line) != nil {
            return ([Token(whole, .rule)], .normal)
        }
        if let heading = match(heading, line) {
            let marks = heading.range(at: 1)
            let level = marks.length
            var tokens = [Token(whole, .heading(level)), Token(0, NSMaxRange(heading.range(at: 2)), .marker)]
            let content = heading.range(at: 3)
            // A closing sequence, `## Title ##`, is muted too.
            if let closing = match(closingHashes, line), closing.range.location >= content.location {
                tokens += inline(line, in: NSRange(location: content.location, length: closing.range.location - content.location))
                tokens.append(Token(closing.range, .marker))
            } else {
                tokens += inline(line, in: content)
            }
            return (tokens, .normal)
        }

        var tokens: [Token] = []
        var rest = whole
        if let quote = match(quote, line) {
            tokens.append(Token(rest.location, quote.range.length, .marker))
            rest = NSRange(location: quote.range.length, length: line.length - quote.range.length)
            tokens.insert(Token(rest, .quote), at: 0)
        }
        if let item = match(listItem, line, in: rest) {
            tokens.append(Token(item.range(at: 1), .listMarker))
            var end = NSMaxRange(item.range)
            let box = item.range(at: 2)
            if box.location != NSNotFound {
                let checked = line.substring(with: NSRange(location: box.location + 1, length: 1)).lowercased() == "x"
                tokens.append(Token(box, .checkbox(checked: checked)))
                end = NSMaxRange(box)
                if checked, NSMaxRange(rest) > end {
                    tokens.append(Token(end, NSMaxRange(rest) - end, .done))
                }
            }
            rest = NSRange(location: end, length: NSMaxRange(rest) - end)
        }
        tokens += inline(line, in: rest)
        return (tokens, .normal)
    }

    // MARK: Inline

    /// The inline syntax of `range` of `line`: code spans, wiki links, links, URLs, tags and
    /// emphasis, in that order of precedence.
    static func inline(_ line: NSString, in range: NSRange) -> [Token] {
        guard range.length > 0 else { return [] }
        var tokens: [Token] = []
        // Characters already claimed by code, a wiki link or a link target: emphasis and tags
        // are not looked for inside them.
        var claimed = IndexSet()

        func free(_ range: NSRange) -> Bool {
            !claimed.intersects(integersIn: range.location ..< NSMaxRange(range))
        }

        func claim(_ range: NSRange) {
            claimed.insert(integersIn: range.location ..< NSMaxRange(range))
        }

        for span in codeSpans(line, in: range) {
            tokens.append(Token(span.open, .marker))
            if span.content.length > 0 { tokens.append(Token(span.content, .code)) }
            tokens.append(Token(span.close, .marker))
            claim(NSUnionRange(span.open, span.close))
        }

        for found in wikiLink.matches(in: line as String, range: range) where free(found.range) {
            let inner = found.range(at: 2)
            let text = line.substring(with: inner)
            let name = text.split(separator: "|", maxSplits: 1).first.map(String.init) ?? text
            let target = name.trimmingCharacters(in: .whitespaces)
            tokens.append(Token(found.range.location, inner.location - found.range.location, .marker))
            tokens.append(Token(inner, .wikiLink(target)))
            tokens.append(Token(NSMaxRange(inner), 2, .marker))
            claim(found.range)
        }

        for found in link.matches(in: line as String, range: range) where free(found.range) {
            let text = found.range(at: 1)
            tokens.append(Token(found.range.location, text.location - found.range.location, .marker))
            if text.length > 0 { tokens.append(Token(text, .link)) }
            tokens.append(Token(NSMaxRange(text), NSMaxRange(found.range) - NSMaxRange(text), .marker))
            // The text of the link can still be emphasised; its target cannot.
            claim(NSRange(location: NSMaxRange(text), length: NSMaxRange(found.range) - NSMaxRange(text)))
        }

        for found in autolink.matches(in: line as String, range: range) where free(found.range) {
            tokens.append(Token(found.range.location, 1, .marker))
            tokens.append(Token(found.range.location + 1, found.range.length - 2, .link))
            tokens.append(Token(NSMaxRange(found.range) - 1, 1, .marker))
            claim(found.range)
        }

        for found in bareURL.matches(in: line as String, range: range) where free(found.range) {
            tokens.append(Token(found.range, .link))
            claim(found.range)
        }

        for found in tag.matches(in: line as String, range: range) where free(found.range) {
            tokens.append(Token(found.range, .tag))
            claim(found.range)
        }

        // Emphasis, outermost first, so `**bold _and italic_**` colours both.
        let emphasis: [(NSRegularExpression, Int, Kind)] = [
            (strongStars, 2, .bold), (strongUnderscores, 2, .bold),
            (strike, 2, .strikethrough),
            (emStars, 1, .italic), (emUnderscores, 1, .italic),
        ]
        var marked = IndexSet()
        for (pattern, width, kind) in emphasis {
            for found in pattern.matches(in: line as String, range: range) {
                let open = NSRange(location: found.range.location, length: width)
                let close = NSRange(location: NSMaxRange(found.range) - width, length: width)
                // Delimiters inside code or a link target, or already used, do not count.
                guard free(open), free(close),
                      !marked.contains(integersIn: open.location ..< NSMaxRange(open)),
                      !marked.contains(integersIn: close.location ..< NSMaxRange(close)) else { continue }
                tokens.append(Token(found.range, kind))
                tokens.append(Token(open, .marker))
                tokens.append(Token(close, .marker))
                marked.insert(integersIn: open.location ..< NSMaxRange(open))
                marked.insert(integersIn: close.location ..< NSMaxRange(close))
            }
        }
        return tokens
    }

    /// Code spans: a run of backticks closed by a run of the same length.
    static func codeSpans(_ line: NSString, in range: NSRange) -> [(open: NSRange, content: NSRange, close: NSRange)] {
        let backtick = unichar(UInt8(ascii: "`"))
        var spans: [(NSRange, NSRange, NSRange)] = []
        var index = range.location
        let end = NSMaxRange(range)

        func run(at start: Int) -> Int {
            var i = start
            while i < end, line.character(at: i) == backtick { i += 1 }
            return i - start
        }

        while index < end {
            guard line.character(at: index) == backtick else { index += 1; continue }
            let width = run(at: index)
            var search = index + width
            var closed = false
            while search < end {
                if line.character(at: search) == backtick {
                    let closing = run(at: search)
                    if closing == width {
                        spans.append((NSRange(location: index, length: width),
                                      NSRange(location: index + width, length: search - index - width),
                                      NSRange(location: search, length: width)))
                        index = search + width
                        closed = true
                        break
                    }
                    search += closing
                } else {
                    search += 1
                }
            }
            if !closed { index += width }
        }
        return spans
    }

    // MARK: Blocks

    private static func fence(_ line: NSString) -> (character: Character, count: Int, info: String)? {
        guard let found = match(fencePattern, line) else { return nil }
        let marks = line.substring(with: found.range(at: 1))
        let info = line.substring(with: found.range(at: 2)).trimmingCharacters(in: .whitespaces)
        return (marks.first ?? "`", marks.count, info)
    }

    private static func match(_ pattern: NSRegularExpression, _ line: NSString, in range: NSRange? = nil) -> NSTextCheckingResult? {
        pattern.firstMatch(in: line as String, range: range ?? NSRange(location: 0, length: line.length))
    }

    private static func regex(_ pattern: String) -> NSRegularExpression {
        // The patterns are constants: a mistake in one is a bug to catch in the tests.
        // swiftlint:disable:next force_try
        try! NSRegularExpression(pattern: pattern)
    }

    nonisolated(unsafe) private static let fencePattern = regex(#"^ {0,3}(`{3,}|~{3,})(.*)$"#)
    nonisolated(unsafe) private static let rule = regex(#"^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$"#)
    nonisolated(unsafe) private static let heading = regex(#"^ {0,3}(#{1,6})([ \t]+|$)(.*)$"#)
    nonisolated(unsafe) private static let closingHashes = regex(#"[ \t]+#+[ \t]*$"#)
    nonisolated(unsafe) private static let quote = regex(#"^ {0,3}>(?:[ \t]?>)*[ \t]?"#)
    nonisolated(unsafe) private static let listItem = regex(#"\G[ \t]*([-*+]|\d{1,9}[.)])(?:[ \t]+|$)(\[[ xX]\](?=[ \t]|$))?"#)
    nonisolated(unsafe) private static let wikiLink = regex(#"(!?\[\[)([^\[\]\n]+?)\]\]"#)
    nonisolated(unsafe) private static let link = regex(#"!?\[([^\[\]]*)\]\([^()\s]*(?:\([^()\s]*\))?[^()\s]*(?:[ \t]+"[^"]*")?\)"#)
    nonisolated(unsafe) private static let autolink = regex(#"<[A-Za-z][A-Za-z0-9+.\-]{1,31}:[^<>\s]*>"#)
    nonisolated(unsafe) private static let bareURL = regex(#"\bhttps?://[^\s<>\[\]()]+[^\s<>\[\]().,;:!?'"]"#)
    nonisolated(unsafe) private static let tag = regex(#"(?<![\p{L}\p{N}_&#/\]])#[\p{L}\p{N}_\-/]*[\p{L}_][\p{L}\p{N}_\-/]*"#)
    nonisolated(unsafe) private static let strongStars = regex(#"\*\*(?=[^\s*])(?:.*?[^\s*])??\*\*"#)
    nonisolated(unsafe) private static let strongUnderscores = regex(#"(?<![\p{L}\p{N}_])__(?=[^\s_])(?:.*?[^\s_])??__(?![\p{L}\p{N}_])"#)
    nonisolated(unsafe) private static let strike = regex(#"~~(?=[^\s~])(?:.*?[^\s~])??~~"#)
    nonisolated(unsafe) private static let emStars = regex(#"(?<!\*)\*(?=[^\s*])(?:.*?[^\s*])??\*(?!\*)"#)
    nonisolated(unsafe) private static let emUnderscores = regex(#"(?<![\p{L}\p{N}_])_(?=[^\s_])(?:.*?[^\s_])??_(?![\p{L}\p{N}_])"#)
}

/// Where a line starts: in ordinary text, in the frontmatter, or in a fenced code block opened by
/// `count` of `character` (a backtick or a tilde).
enum MarkdownLineState: Hashable, Sendable {
    case normal
    case frontmatter
    case fence(Character, Int)

    /// The state as a number, to keep it in a text storage attribute.
    var code: Int {
        switch self {
        case .normal: 0
        case .frontmatter: 1
        case .fence(let character, let count): (character == "~" ? 2 : 3) + 2 * min(count, 1_000)
        }
    }

    init?(code: Int) {
        switch code {
        case 0: self = .normal
        case 1: self = .frontmatter
        case 2...: self = .fence(code % 2 == 0 ? "~" : "`", (code - (code % 2 == 0 ? 2 : 3)) / 2)
        default: return nil
        }
    }
}
