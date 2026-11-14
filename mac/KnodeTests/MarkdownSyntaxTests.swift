import AppKit
import Foundation
import Testing
@testable import Knode

@Suite struct MarkdownSyntaxTests {
    typealias Kind = MarkdownSyntax.Kind

    /// The text of each token of `line`, with its kind, in order.
    private func spans(_ line: String, from state: MarkdownLineState = .normal, first: Bool = false) -> [(String, Kind)] {
        let text = line as NSString
        return MarkdownSyntax.tokenize(line, from: state, isFirstLine: first).tokens.map { (text.substring(with: $0.range), $0.kind) }
    }

    private func has(_ line: String, _ text: String, _ kind: Kind, from state: MarkdownLineState = .normal) -> Bool {
        spans(line, from: state).contains { $0.0 == text && $0.1 == kind }
    }

    @Test func headingsCarryTheirLevelAndMuteTheMarkers() {
        #expect(has("## Title", "## Title", .heading(2)))
        #expect(has("## Title", "## ", .marker))
        #expect(has("###### Six", "###### ", .marker))
        #expect(has("# Title #", " #", .marker))
        #expect(spans("#tag").map(\.1) == [.tag])
        #expect(spans("####### seven").allSatisfy { if case .heading = $0.1 { false } else { true } })
    }

    @Test func emphasisAndItsMarkers() {
        let line = "a **bold** and _it_ and *it* and ~~gone~~"
        #expect(has(line, "**bold**", .bold))
        #expect(has(line, "_it_", .italic))
        #expect(has(line, "*it*", .italic))
        #expect(has(line, "~~gone~~", .strikethrough))
        let markers = spans(line).filter { $0.1 == .marker }.map(\.0)
        #expect(markers.sorted() == ["*", "*", "**", "**", "_", "_", "~~", "~~"])
    }

    @Test func underscoresInsideWordsAreNotEmphasis() {
        #expect(spans("snake_case_name").isEmpty)
        #expect(spans("2 * 3 * 4").isEmpty)
    }

    @Test func codeSpansHideTheirContent() {
        let line = "run `a **b**` now"
        #expect(has(line, "a **b**", .code))
        #expect(!spans(line).contains { $0.1 == .bold })
        #expect(spans(line).filter { $0.1 == .marker }.map(\.0) == ["`", "`"])
        #expect(has("``a ` b``", "a ` b", .code))
        #expect(spans("an `unclosed span").isEmpty)
    }

    @Test func linksAndWikiLinks() {
        let line = "See [the site](https://knode.md/a_b_c) and [[Other note|alias]] or ![[photo.png]]"
        #expect(has(line, "the site", .link))
        #expect(has(line, "](https://knode.md/a_b_c)", .marker))
        #expect(!spans(line).contains { $0.1 == .italic })
        #expect(has(line, "Other note|alias", .wikiLink("Other note")))
        #expect(has(line, "photo.png", .wikiLink("photo.png")))
        #expect(has(line, "![[", .marker))
        #expect(has("go to https://knode.md/docs.", "https://knode.md/docs", .link))
        #expect(has("<https://knode.md>", "https://knode.md", .link))
    }

    @Test func tags() {
        #expect(has("about #knode/mac today", "#knode/mac", .tag))
        #expect(spans("issue #123").isEmpty)
        #expect(spans("a&#38;b").isEmpty)
    }

    @Test func listsCheckboxesAndQuotes() {
        #expect(has("- item", "-", .listMarker))
        #expect(has("  12. item", "12.", .listMarker))
        #expect(has("- [ ] todo", "[ ]", .checkbox(checked: false)))
        #expect(has("- [x] done", "[x]", .checkbox(checked: true)))
        #expect(has("- [x] done", " done", .done))
        #expect(has("> quoted **bold**", "> ", .marker))
        #expect(has("> quoted **bold**", "quoted **bold**", .quote))
        #expect(has("> quoted **bold**", "**bold**", .bold))
        #expect(has("> - in a quote", "-", .listMarker))
        #expect(has("---", "---", .rule))
        #expect(has("* * *", "* * *", .rule))
    }

    @Test func fencedCodeBlocksCarryTheirStateAcrossLines() {
        var state = MarkdownLineState.normal
        var kinds: [[Kind]] = []
        for line in ["```swift", "let a = **b**", "~~~", "```", "after **x**"] {
            let result = MarkdownSyntax.tokenize(line, from: state)
            kinds.append(result.tokens.map(\.kind))
            state = result.state
        }
        #expect(kinds[0] == [.codeBlock, .marker])
        #expect(kinds[1] == [.codeBlock])
        // A tilde fence does not close a backtick one.
        #expect(kinds[2] == [.codeBlock])
        #expect(kinds[3] == [.codeBlock, .marker])
        #expect(kinds[4].contains(.bold))
        #expect(state == .normal)
    }

    @Test func frontmatterOnlyAtTheTop() {
        let open = MarkdownSyntax.tokenize("---", from: .normal, isFirstLine: true)
        #expect(open.state == .frontmatter)
        let inside = MarkdownSyntax.tokenize("title: x", from: open.state)
        #expect(inside.tokens.map(\.kind) == [.frontmatter])
        #expect(MarkdownSyntax.tokenize("---", from: inside.state).state == .normal)
        #expect(MarkdownSyntax.tokenize("---", from: .normal, isFirstLine: false).tokens.map(\.kind) == [.rule])
    }

    @Test func stateCodesRoundTrip() {
        for state in [MarkdownLineState.normal, .frontmatter, .fence("`", 3), .fence("~", 4), .fence("`", 12)] {
            #expect(MarkdownLineState(code: state.code) == state)
        }
    }

    @Test func rangesAreInUTF16() {
        let line = "é 🧠 **bold**"
        let token = MarkdownSyntax.tokenize(line, from: .normal).tokens.first { $0.kind == .bold }
        #expect(token?.range == NSRange(location: 5, length: 8))
    }
}

@MainActor
@Suite struct MarkdownHighlighterTests {
    private static let sample = """
    ---
    title: Sample
    ---
    # Heading with `code`

    Some **bold** text and a [[Link]] with #tag.

    ```
    code **not bold**
    ```

    - [x] done
    > quote
    """

    /// A storage with the highlighter as its delegate, styled from `text`.
    private func storage(_ text: String, highlighter: MarkdownHighlighter) -> NSTextStorage {
        let storage = NSTextStorage()
        storage.delegate = highlighter
        storage.replaceCharacters(in: NSRange(location: 0, length: 0), with: text)
        return storage
    }

    /// Whether `edited`, styled edit by edit, is styled exactly as its text styled at once.
    private func matchesFreshStyling(_ edited: NSTextStorage) -> Bool {
        let fresh = storage(edited.string, highlighter: MarkdownHighlighter())
        return fresh.isEqual(to: edited)
    }

    private func attribute(_ key: NSAttributedString.Key, of text: String, in storage: NSTextStorage) -> Any? {
        let range = (storage.string as NSString).range(of: text)
        return storage.attribute(key, at: range.location, effectiveRange: nil)
    }

    @Test func stylesTheWholeNote() {
        let highlighter = MarkdownHighlighter()
        let styled = storage(Self.sample, highlighter: highlighter)
        #expect(attribute(.font, of: "Heading", in: styled) as? NSFont == MarkdownStyle.heading(1))
        #expect(attribute(.font, of: "bold", in: styled) as? NSFont == MarkdownStyle.bold)
        #expect(attribute(.foregroundColor, of: "**", in: styled) as? NSColor == MarkdownStyle.muted)
        #expect(attribute(.knodeWikiLink, of: "Link", in: styled) as? String == "Link")
        #expect(attribute(.foregroundColor, of: "#tag", in: styled) as? NSColor == MarkdownStyle.link)
        #expect(attribute(.foregroundColor, of: "title", in: styled) as? NSColor == MarkdownStyle.muted)
        #expect(attribute(.backgroundColor, of: "code **not", in: styled) as? NSColor == MarkdownStyle.panel)
        #expect(attribute(.font, of: "not bold", in: styled) as? NSFont == MarkdownStyle.font)
    }

    @Test func editsRestyleAsAFreshStyleWould() {
        let highlighter = MarkdownHighlighter()
        let styled = storage(Self.sample, highlighter: highlighter)
        func edit(_ range: NSRange, _ text: String) {
            styled.replaceCharacters(in: range, with: text)
            #expect(matchesFreshStyling(styled), "after replacing \(range) with \(text.debugDescription)")
        }
        let text = { styled.string as NSString }
        // Typing emphasis, character by character.
        let at = text().range(of: "Some").location
        for (offset, character) in "**new** ".enumerated() {
            edit(NSRange(location: at + offset, length: 0), String(character))
        }
        // Opening a code block makes the rest code; closing it gives the rest back.
        edit(NSRange(location: at, length: 0), "```\n")
        #expect(attribute(.backgroundColor, of: "> quote", in: styled) as? NSColor == MarkdownStyle.panel)
        edit(NSRange(location: at, length: 4), "")
        // Removing the frontmatter's opening line.
        edit(NSRange(location: 0, length: 4), "")
        edit(NSRange(location: 0, length: 0), "---\n")
        // Joining two lines, and a remote patch over several lines.
        let blank = text().range(of: "\n\n").location
        edit(NSRange(location: blank, length: 1), "")
        edit(NSRange(location: 0, length: text().length / 2), "# New top\n\n```\nopen")
    }

    @Test func linesAfterAnUnchangedStateAreLeftAlone() {
        let highlighter = MarkdownHighlighter()
        let lines = (0 ..< 2_000).map { "Line \($0) with **bold** and [[link \($0)]]" }
        let styled = storage(lines.joined(separator: "\n"), highlighter: highlighter)
        // Colour the last line by hand: an edit at the top must not restyle it.
        let last = (styled.string as NSString).range(of: "Line 1999")
        styled.addAttribute(.foregroundColor, value: NSColor.red, range: last)
        let clock = ContinuousClock()
        let typing = clock.measure {
            styled.replaceCharacters(in: NSRange(location: 0, length: 0), with: "x")
        }
        // One keystroke in a long note restyles one line, not the note.
        #expect(typing < .milliseconds(20))
        #expect(styled.attribute(.foregroundColor, at: last.location + 1, effectiveRange: nil) as? NSColor == NSColor.red)
        // An edit that opens a code block reaches it.
        styled.replaceCharacters(in: NSRange(location: 0, length: 0), with: "```\n")
        #expect(styled.attribute(.foregroundColor, at: last.location + 5, effectiveRange: nil) as? NSColor == MarkdownStyle.fg)
    }

    @Test func waitsWhileComposing() {
        let highlighter = MarkdownHighlighter()
        let styled = storage("plain", highlighter: highlighter)
        var composing = true
        highlighter.isComposing = { composing }
        styled.replaceCharacters(in: NSRange(location: 5, length: 0), with: " **b**")
        #expect(attribute(.font, of: "b**", in: styled) as? NSFont == MarkdownStyle.font)
        composing = false
        highlighter.finishComposing(styled)
        #expect(matchesFreshStyling(styled))
    }
}
