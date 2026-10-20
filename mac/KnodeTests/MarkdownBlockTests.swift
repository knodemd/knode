import Foundation
import Testing
@testable import Knode

@Suite struct MarkdownBlockTests {
    @Test func parsesTheCommonBlocks() {
        let markdown = """
        # Title

        First line
        continues here.

        > A quote

        - item
        - [ ] todo
        - [x] done
        1. one
        ---
        ```swift
        let x = 1
        ```
        """
        #expect(MarkdownBlock.parse(markdown) == [
            .heading(level: 1, text: "Title"),
            .paragraph("First line continues here."),
            .quote("A quote"),
            .bullet("item", checked: nil),
            .bullet("todo", checked: false),
            .bullet("done", checked: true),
            .numbered(1, "one"),
            .rule,
            .code(language: "swift", text: "let x = 1"),
        ])
    }

    @Test func hashWithoutSpaceIsNotAHeading() {
        #expect(MarkdownBlock.parse("#tag") == [.paragraph("#tag")])
    }

    @Test func unclosedFenceKeepsItsCode() {
        #expect(MarkdownBlock.parse("```\nopen") == [.code(language: nil, text: "open")])
    }

    @Test func wikiLinksBecomeNodeLinks() throws {
        #expect(InlineMarkdown.wikiLinks(in: "See [[Two Words]].") == "See [Two Words](knode://node/Two%20Words).")
        #expect(InlineMarkdown.wikiLinks(in: "Code `[[kept]]` here") == "Code `[[kept]]` here")
        #expect(InlineMarkdown.wikiLinks(in: "In [[SQLite for local data|SQLite]].") == "In [SQLite](knode://node/SQLite%20for%20local%20data).")
        let url = try #require(URL(string: "knode://node/Two%20Words"))
        #expect(InlineMarkdown.linkedTitle(url) == "Two Words")
        #expect(InlineMarkdown.linkedTitle(URL(string: "https://knode.md")!) == nil)
    }

    @Test func rendersInlineMarkdown() {
        let rendered = InlineMarkdown.render("Some **bold** and [[Note]]")
        #expect(String(rendered.characters) == "Some bold and Note")
        #expect(rendered.runs.contains { $0.link?.scheme == "knode" })
    }
}
