import Foundation

/// One splice of a note's body as Knode Service takes it (`doc.edit`): at `at`, delete `delete`
/// characters and insert `insert`. Positions and lengths count Unicode scalars, not UTF-16 units.
public struct TextSplice: Hashable, Sendable, Codable {
    public var at: Int
    public var delete: Int
    public var insert: String

    public init(at: Int, delete: Int, insert: String) {
        self.at = at
        self.delete = delete
        self.insert = insert
    }
}

/// One replacement in a text as `NSTextView` counts it: `range`, in UTF-16 units of the text
/// before the change, becomes `replacement`.
public struct TextPatch: Hashable, Sendable {
    public var range: NSRange
    public var replacement: String

    public init(range: NSRange, replacement: String) {
        self.range = range
        self.replacement = replacement
    }
}

/// Differences between two texts, and how a selection follows them.
public enum TextDiff {
    /// Above this many characters in the part that differs, texts are compared line by line,
    /// then character by character inside the lines that changed.
    static let characterLimit = 20_000

    /// The patches that turn `old` into `new`, in ascending order and in the positions of `old`.
    /// They never split a Unicode scalar.
    public static func patches(from old: String, to new: String) -> [TextPatch] {
        let oldScalars = Array(old.unicodeScalars)
        let edits = scalarEdits(oldScalars, Array(new.unicodeScalars))
        guard !edits.isEmpty else { return [] }
        // UTF-16 offset of each scalar position of `old`.
        var offsets = [Int](repeating: 0, count: oldScalars.count + 1)
        for (index, scalar) in oldScalars.enumerated() {
            offsets[index + 1] = offsets[index] + scalar.utf16.count
        }
        return edits.map { edit in
            let start = offsets[edit.at]
            return TextPatch(
                range: NSRange(location: start, length: offsets[edit.at + edit.delete] - start),
                replacement: String(String.UnicodeScalarView(edit.insert))
            )
        }
    }

    /// The splices that turn `old` into `new`, to be applied one after another as `doc.edit`
    /// applies them: from the end of the text backwards, so each position is still right.
    public static func splices(from old: String, to new: String) -> [TextSplice] {
        scalarEdits(Array(old.unicodeScalars), Array(new.unicodeScalars)).reversed().map {
            TextSplice(at: $0.at, delete: $0.delete, insert: String(String.UnicodeScalarView($0.insert)))
        }
    }

    /// The splice an edit made in an `NSTextView` stands for: `range` (UTF-16, in `old`) replaced
    /// by `replacement`. `nil` when the range does not fall on Unicode scalar boundaries.
    public static func splice(in old: String, replacing range: NSRange, with replacement: String) -> TextSplice? {
        let utf16 = old.utf16
        guard range.location >= 0, NSMaxRange(range) <= utf16.count,
              let start = utf16.index(utf16.startIndex, offsetBy: range.location, limitedBy: utf16.endIndex),
              let end = utf16.index(start, offsetBy: range.length, limitedBy: utf16.endIndex),
              let scalarStart = start.samePosition(in: old.unicodeScalars),
              let scalarEnd = end.samePosition(in: old.unicodeScalars)
        else { return nil }
        let scalars = old.unicodeScalars
        return TextSplice(
            at: scalars.distance(from: scalars.startIndex, to: scalarStart),
            delete: scalars.distance(from: scalarStart, to: scalarEnd),
            insert: replacement
        )
    }

    /// Applies splices one after another, as `doc.edit` does.
    public static func apply(_ splices: [TextSplice], to text: String) -> String {
        var scalars = Array(text.unicodeScalars)
        for splice in splices {
            let at = min(max(splice.at, 0), scalars.count)
            let end = min(at + max(splice.delete, 0), scalars.count)
            scalars.replaceSubrange(at..<end, with: Array(splice.insert.unicodeScalars))
        }
        return String(String.UnicodeScalarView(scalars))
    }

    /// Applies patches in the positions of `text`, as `patches(from:to:)` makes them.
    public static func apply(_ patches: [TextPatch], to text: String) -> String {
        let string = NSMutableString(string: text)
        for patch in patches.sorted(by: { $0.range.location > $1.range.location }) {
            string.replaceCharacters(in: patch.range, with: patch.replacement)
        }
        return string as String
    }

    /// Where a position in the old text is after the patches. A position inside a replaced run
    /// keeps its offset into the replacement, as far as the replacement goes; text inserted
    /// right at the position goes after it.
    public static func transform(_ position: Int, through patches: [TextPatch]) -> Int {
        var shift = 0
        for patch in patches {
            let start = patch.range.location
            let end = NSMaxRange(patch.range)
            let inserted = patch.replacement.utf16.count
            if position < start || (position == start && patch.range.length == 0) { break }
            if position >= end {
                shift += inserted - patch.range.length
                continue
            }
            return start + shift + min(position - start, inserted)
        }
        return position + shift
    }

    /// Where a selection in the old text is after the patches.
    public static func transform(_ range: NSRange, through patches: [TextPatch]) -> NSRange {
        let start = transform(range.location, through: patches)
        if range.length == 0 { return NSRange(location: start, length: 0) }
        let end = transform(NSMaxRange(range), through: patches)
        return NSRange(location: start, length: max(0, end - start))
    }

    // MARK: Diff

    struct Edit: Equatable {
        var at: Int
        var delete: Int
        var insert: [Unicode.Scalar]
    }

    /// The edits that turn `old` into `new`: ascending, in the positions of `old`, not touching.
    static func scalarEdits(_ old: [Unicode.Scalar], _ new: [Unicode.Scalar]) -> [Edit] {
        var prefix = 0
        while prefix < old.count, prefix < new.count, old[prefix] == new[prefix] { prefix += 1 }
        var suffix = 0
        while suffix < old.count - prefix, suffix < new.count - prefix,
              old[old.count - 1 - suffix] == new[new.count - 1 - suffix] { suffix += 1 }
        let oldMiddle = Array(old[prefix..<(old.count - suffix)])
        let newMiddle = Array(new[prefix..<(new.count - suffix)])
        if oldMiddle.isEmpty, newMiddle.isEmpty { return [] }
        if oldMiddle.isEmpty || newMiddle.isEmpty {
            return [Edit(at: prefix, delete: oldMiddle.count, insert: newMiddle)]
        }
        let edits = oldMiddle.count + newMiddle.count > characterLimit
            ? lineEdits(oldMiddle, newMiddle)
            : elementEdits(oldMiddle, newMiddle).map { Edit(at: $0.at, delete: $0.delete, insert: $0.insert) }
        return edits.map { Edit(at: $0.at + prefix, delete: $0.delete, insert: $0.insert) }
    }

    /// A Myers diff of two sequences, as runs of removed and inserted elements.
    private static func elementEdits<T: Hashable>(_ old: [T], _ new: [T]) -> [(at: Int, delete: Int, insert: [T])] {
        let difference = new.difference(from: old)
        var removed = Set<Int>()
        var inserted = Set<Int>()
        for change in difference {
            switch change {
            case .remove(let offset, _, _): removed.insert(offset)
            case .insert(let offset, _, _): inserted.insert(offset)
            }
        }
        var edits: [(at: Int, delete: Int, insert: [T])] = []
        var i = 0, j = 0
        while i < old.count || j < new.count {
            if i < old.count, j < new.count, !removed.contains(i), !inserted.contains(j) {
                i += 1
                j += 1
                continue
            }
            let start = i
            var insert: [T] = []
            while (i < old.count && removed.contains(i)) || (j < new.count && inserted.contains(j)) {
                if i < old.count, removed.contains(i) {
                    i += 1
                } else {
                    insert.append(new[j])
                    j += 1
                }
            }
            edits.append((start, i - start, insert))
        }
        return edits
    }

    /// Compares long texts line by line, then character by character inside each run of changed
    /// lines that is short enough.
    private static func lineEdits(_ old: [Unicode.Scalar], _ new: [Unicode.Scalar]) -> [Edit] {
        let oldLines = lines(old)
        let newLines = lines(new)
        var oldStarts = [0]
        for line in oldLines { oldStarts.append(oldStarts.last! + line.count) }
        var edits: [Edit] = []
        var newIndex = 0
        var oldIndex = 0
        for run in elementEdits(oldLines, newLines) {
            // Lines kept before the run.
            newIndex += run.at - oldIndex
            oldIndex = run.at
            let removed = Array(oldLines[run.at..<(run.at + run.delete)].joined())
            let added = Array(run.insert.joined())
            let start = oldStarts[run.at]
            if !removed.isEmpty, !added.isEmpty, removed.count + added.count <= characterLimit {
                edits += elementEdits(removed, added).map { Edit(at: start + $0.at, delete: $0.delete, insert: $0.insert) }
            } else {
                edits.append(Edit(at: start, delete: removed.count, insert: added))
            }
            oldIndex += run.delete
            newIndex += run.insert.count
        }
        return edits
    }

    /// The lines of a text, each with its line break.
    private static func lines(_ text: [Unicode.Scalar]) -> [[Unicode.Scalar]] {
        var lines: [[Unicode.Scalar]] = []
        var current: [Unicode.Scalar] = []
        for scalar in text {
            current.append(scalar)
            if scalar == "\n" {
                lines.append(current)
                current = []
            }
        }
        if !current.isEmpty { lines.append(current) }
        return lines
    }
}
