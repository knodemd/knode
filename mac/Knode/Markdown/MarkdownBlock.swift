import Foundation

/// A Markdown body split into the blocks the preview draws. Inline Markdown (emphasis, code, links)
/// stays in `text` and is rendered with `AttributedString(markdown:)`.
///
/// This is only for the preview; the core owns the real parser (`knode-md`).
enum MarkdownBlock: Equatable {
    case heading(level: Int, text: String)
    case paragraph(String)
    case quote(String)
    case bullet(String, checked: Bool?)
    case numbered(Int, String)
    case code(language: String?, text: String)
    case rule
    /// `![[name]]` on a line of its own: an attachment of the note shown where it is written,
    /// such as an image or a PDF.
    case embed(String)
    /// A GitHub table: its header cells and its rows, inline Markdown in each cell.
    case table(header: [String], rows: [[String]])
    /// An HTML `<img>` on a line of its own, such as a README banner: its description and
    /// address. The preview never loads it from the network; it shows a link to it.
    case image(alt: String, source: String)

    static func parse(_ markdown: String) -> [MarkdownBlock] {
        var blocks: [MarkdownBlock] = []
        var paragraph: [String] = []
        var quote: [String] = []
        var fence: (language: String?, lines: [String])?
        var table: [String] = []

        func flush() {
            if !table.isEmpty {
                if let parsed = Self.table(table) {
                    blocks.append(parsed)
                } else {
                    paragraph = table + paragraph
                }
                table = []
            }
            if !paragraph.isEmpty { blocks.append(.paragraph(paragraph.joined(separator: " "))) }
            if !quote.isEmpty { blocks.append(.quote(quote.joined(separator: " "))) }
            paragraph = []
            quote = []
        }

        for rawLine in markdown.components(separatedBy: .newlines) {
            let line = rawLine.trimmingCharacters(in: .whitespaces)

            if var open = fence {
                if line.hasPrefix("```") {
                    blocks.append(.code(language: open.language, text: open.lines.joined(separator: "\n")))
                    fence = nil
                } else {
                    open.lines.append(rawLine)
                    fence = open
                }
                continue
            }

            if line.hasPrefix("|") {
                // A table may interrupt a paragraph, as on GitHub.
                if table.isEmpty { flush() }
                table.append(line)
                continue
            } else if !table.isEmpty {
                // Not a table after all: its lines are text the next line may continue.
                if let parsed = Self.table(table) { blocks.append(parsed) } else { paragraph += table }
                table = []
            }

            if line.hasPrefix("```") {
                flush()
                let language = line.dropFirst(3).trimmingCharacters(in: .whitespaces)
                fence = (language.isEmpty ? nil : language, [])
            } else if line.isEmpty {
                flush()
            } else if let heading = heading(line) {
                flush()
                blocks.append(heading)
            } else if let names = embeds(line) {
                flush()
                blocks += names.map(MarkdownBlock.embed)
            } else if line.hasPrefix("<"), let html = html(line) {
                flush()
                blocks += html
            } else if line.hasPrefix("<"), Self.tagsOnly(line) {
                // A line of HTML tags alone, such as `<p align="center">` or `</div>`: nothing to show.
                flush()
            } else if line == "---" || line == "***" || line == "___" {
                flush()
                blocks.append(.rule)
            } else if line.hasPrefix(">") {
                if !paragraph.isEmpty { flush() }
                quote.append(line.dropFirst().trimmingCharacters(in: .whitespaces))
            } else if let item = bullet(line) {
                flush()
                blocks.append(item)
            } else if let item = numbered(line) {
                flush()
                blocks.append(item)
            } else {
                if !quote.isEmpty { flush() }
                paragraph.append(line)
            }
        }
        if let open = fence {
            blocks.append(.code(language: open.language, text: open.lines.joined(separator: "\n")))
        }
        flush()
        return blocks
    }

    private static func heading(_ line: String) -> MarkdownBlock? {
        let hashes = line.prefix(while: { $0 == "#" }).count
        guard (1...6).contains(hashes) else { return nil }
        let rest = line.dropFirst(hashes)
        guard rest.first == " " else { return nil }
        return .heading(level: hashes, text: rest.trimmingCharacters(in: .whitespaces))
    }

    /// The names of a line made only of `![[name]]` embeds, or nil. What follows `|` in an embed,
    /// such as a size, is left out.
    static func embeds(_ line: String) -> [String]? {
        var names: [String] = []
        var rest = Substring(line)
        while true {
            rest = rest.drop(while: \.isWhitespace)
            if rest.isEmpty { break }
            guard rest.hasPrefix("![["), let close = rest.range(of: "]]") else { return nil }
            let inner = rest[rest.index(rest.startIndex, offsetBy: 3)..<close.lowerBound]
            let name = (inner.split(separator: "|", maxSplits: 1, omittingEmptySubsequences: false).first ?? inner)
                .trimmingCharacters(in: .whitespaces)
            guard !name.isEmpty, !name.contains("[") else { return nil }
            names.append(name)
            rest = rest[close.upperBound...]
        }
        return names.isEmpty ? nil : names
    }

    /// A table from its lines, when the second is a delimiter row such as `|---|:--:|`.
    static func table(_ lines: [String]) -> MarkdownBlock? {
        guard lines.count >= 2 else { return nil }
        let delimiter = cells(lines[1])
        guard !delimiter.isEmpty, delimiter.allSatisfy({ cell in
            !cell.isEmpty && cell.allSatisfy { "-:".contains($0) } && cell.contains("-")
        }) else { return nil }
        let header = cells(lines[0])
        let rows = lines.dropFirst(2).map { row in
            let values = cells(row)
            return (0..<header.count).map { $0 < values.count ? values[$0] : "" }
        }
        return .table(header: header, rows: rows)
    }

    /// The cells of a table row, trimmed; `\|` stays in the cell as `|`.
    static func cells(_ line: String) -> [String] {
        var row = line.trimmingCharacters(in: .whitespaces)
        if row.hasPrefix("|") { row.removeFirst() }
        if row.hasSuffix("|"), !row.hasSuffix("\\|") { row.removeLast() }
        var cells: [String] = []
        var cell = ""
        var escaped = false
        for character in row {
            if escaped {
                cell.append(character == "|" ? "|" : "\\\(character)")
                escaped = false
            } else if character == "\\" {
                escaped = true
            } else if character == "|" {
                cells.append(cell.trimmingCharacters(in: .whitespaces))
                cell = ""
            } else {
                cell.append(character)
            }
        }
        if escaped { cell.append("\\") }
        cells.append(cell.trimmingCharacters(in: .whitespaces))
        return cells
    }

    /// The `<img>` tags of a line of HTML, or nil when it has none.
    static func html(_ line: String) -> [MarkdownBlock]? {
        var images: [MarkdownBlock] = []
        var rest = Substring(line)
        while let open = rest.range(of: "<img", options: .caseInsensitive) {
            guard let close = rest[open.upperBound...].range(of: ">") else { break }
            let tag = rest[open.lowerBound..<close.upperBound]
            if let source = attribute("src", in: tag) {
                images.append(.image(alt: attribute("alt", in: tag) ?? "", source: source))
            }
            rest = rest[close.upperBound...]
        }
        return images.isEmpty ? nil : images
    }

    /// Whether a line is made only of HTML tags, with no text between them.
    static func tagsOnly(_ line: String) -> Bool {
        var text = ""
        var inside = false
        for character in line {
            if character == "<" { inside = true } else if character == ">" { inside = false } else if !inside { text.append(character) }
        }
        return text.trimmingCharacters(in: .whitespaces).isEmpty && line.contains(">")
    }

    private static func attribute(_ name: String, in tag: Substring) -> String? {
        for quote in ["\"", "'"] {
            if let start = tag.range(of: "\(name)=\(quote)", options: .caseInsensitive),
               let end = tag[start.upperBound...].range(of: quote) {
                return String(tag[start.upperBound..<end.lowerBound])
            }
        }
        return nil
    }

    private static func bullet(_ line: String) -> MarkdownBlock? {
        guard let marker = line.first, "-*+".contains(marker), line.dropFirst().first == " " else { return nil }
        let text = line.dropFirst(2)
        for (box, checked) in [("[ ] ", false), ("[x] ", true), ("[X] ", true)] where text.hasPrefix(box) {
            return .bullet(String(text.dropFirst(box.count)), checked: checked)
        }
        return .bullet(String(text), checked: nil)
    }

    private static func numbered(_ line: String) -> MarkdownBlock? {
        let digits = line.prefix(while: \.isNumber)
        guard !digits.isEmpty, let number = Int(digits) else { return nil }
        let rest = line.dropFirst(digits.count)
        guard rest.hasPrefix(". ") || rest.hasPrefix(") ") else { return nil }
        return .numbered(number, String(rest.dropFirst(2)))
    }
}

enum InlineMarkdown {
    /// Inline Markdown as an attributed string. `[[Title]]` links become `knode://node/Title` links
    /// so the preview can follow them.
    static func render(_ text: String) -> AttributedString {
        let linked = wikiLinks(in: text)
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: linked, options: options)) ?? AttributedString(text)
    }

    /// Rewrites `[[Title]]` as a Markdown link to `knode://node/<Title>`.
    static func wikiLinks(in text: String) -> String {
        // Odd segments are inline code, where `[[...]]` stays as written.
        text.components(separatedBy: "`").enumerated()
            .map { index, segment in index.isMultiple(of: 2) ? linkSegment(segment) : segment }
            .joined(separator: "`")
    }

    private static func linkSegment(_ text: String) -> String {
        var output = ""
        var rest = Substring(text)
        while let open = rest.range(of: "[["), let close = rest[open.upperBound...].range(of: "]]") {
            output += rest[..<open.lowerBound]
            // `[[Title|shown text]]` links to Title and reads as the text after the bar.
            let inner = rest[open.upperBound..<close.lowerBound]
            let parts = inner.split(separator: "|", maxSplits: 1, omittingEmptySubsequences: false)
            let title = String(parts[0])
            let shown = parts.count > 1 && !parts[1].isEmpty ? String(parts[1]) : title
            let escaped = title.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? title
            output += "[\(shown)](knode://node/\(escaped))"
            rest = rest[close.upperBound...]
        }
        return output + rest
    }

    /// The title a `knode://node/<Title>` link points to.
    static func linkedTitle(_ url: URL) -> String? {
        guard url.scheme == "knode", url.host() == "node" else { return nil }
        let path = url.path(percentEncoded: false)
        return path.hasPrefix("/") ? String(path.dropFirst()) : path
    }
}
