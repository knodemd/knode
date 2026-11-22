import Foundation

/// Why minutes cannot be written on this Mac now.
enum SummarizerUnavailableReason: Hashable, Sendable {
    case appleIntelligenceNotEnabled
    case deviceNotEligible
    case modelNotReady
    case unsupportedLanguage(String)
    case other(String)

    var message: String {
        switch self {
        case .appleIntelligenceNotEnabled:
            "Turn on Apple Intelligence in System Settings to write minutes on this Mac. The transcript is kept."
        case .deviceNotEligible:
            "This Mac cannot run Apple's on-device model, so no minutes are written. The transcript is kept."
        case .modelNotReady:
            "Apple's on-device model is still downloading or getting ready. Try again later; the transcript is kept."
        case .unsupportedLanguage(let language):
            "Apple's on-device model does not write in \(language) yet. The transcript is kept."
        case .other(let reason):
            "Minutes cannot be written on this Mac: \(reason). The transcript is kept."
        }
    }
}

enum SummarizerAvailability: Hashable, Sendable {
    case available
    case unavailable(SummarizerUnavailableReason)
}

enum SummarizerError: Error, LocalizedError, Equatable {
    case unavailable(SummarizerUnavailableReason)
    case emptyTranscript

    var errorDescription: String? {
        switch self {
        case .unavailable(let reason): reason.message
        case .emptyTranscript: "The transcript is empty, so there is nothing to write minutes from."
        }
    }
}

/// Writes the minutes of a meeting from its transcript, on the device.
protocol Summarizer: Sendable {
    func availability(for transcript: Transcript) async -> SummarizerAvailability
    /// The minutes; `progress` goes from 0 to 1. Cancelling the task stops it.
    func summarize(_ transcript: Transcript, progress: @escaping @Sendable (Double) -> Void) async throws -> MeetingSummary
}

/// What one stretch of a transcript says.
struct ChunkMinutes: Hashable, Sendable {
    var keyPoints: [String] = []
    var decisions: [String] = []
    var actionItems: [ActionItem] = []
    var openQuestions: [String] = []
}

/// A model that reads one stretch of transcript at a time. Tests use fakes.
protocol MinutesModel: Sendable {
    /// The name recorded as the minutes' author.
    var name: String { get }
    /// How many characters of this transcript one call may read: fewer for scripts, such as
    /// Chinese or Japanese, that take a token or more a character.
    func inputBudget(for text: String) async -> Int
    func availability(language: String) async -> SummarizerAvailability
    /// Throws `MinutesModelError.contextTooLarge` when `chunk` does not fit after all, and
    /// `answerTooLong` when the answer was cut off before it was complete.
    func extract(_ chunk: String, language: String) async throws -> ChunkMinutes
    /// An overview paragraph or two from these key points.
    func overview(of keyPoints: [String], language: String) async throws -> String
}

enum MinutesModelError: Error, Equatable {
    /// The prompt does not fit the model's context.
    case contextTooLarge
    /// The answer reached the most tokens it may have and was cut off.
    case answerTooLong
}

/// Minutes by map-reduce, so a long meeting fits a small context: the transcript is cut into
/// chunks at turn boundaries, each chunk's key points, decisions, action items and open questions
/// are extracted, and the overview is written from the key points (in rounds when they are too
/// many). Every decision, action item and open question of every chunk is kept; only exact or
/// near-exact repeats are merged, in code.
struct MapReduceSummarizer<Model: MinutesModel>: Summarizer {
    var model: Model

    func availability(for transcript: Transcript) async -> SummarizerAvailability {
        await model.availability(language: transcript.locale)
    }

    func summarize(_ transcript: Transcript, progress: @escaping @Sendable (Double) -> Void) async throws -> MeetingSummary {
        if case .unavailable(let reason) = await availability(for: transcript) {
            throw SummarizerError.unavailable(reason)
        }
        let budget = await model.inputBudget(for: TranscriptChunker.turns(transcript.segments).joined(separator: "\n"))
        let chunks = TranscriptChunker.chunks(transcript.segments, budget: budget)
        guard !chunks.isEmpty else { throw SummarizerError.emptyTranscript }
        let language = transcript.locale

        var extracted: [ChunkMinutes] = []
        for (index, chunk) in chunks.enumerated() {
            try Task.checkCancellation()
            extracted += try await extract(chunk, language: language)
            progress(Double(index + 1) / Double(chunks.count + 1) * 0.9)
        }
        let merged = MinutesMerger.merge(extracted)
        try Task.checkCancellation()
        let overview = try await overview(of: merged.keyPoints, language: language, budget: budget)
        progress(1)
        return MeetingSummary(
            model: model.name,
            overview: overview,
            keyPoints: merged.keyPoints,
            decisions: merged.decisions,
            actionItems: merged.actionItems,
            openQuestions: merged.openQuestions
        )
    }

    /// Extracts a chunk, halving it while the model says it does not fit or its answer was cut
    /// off: half the transcript has half as much to say.
    private func extract(_ chunk: String, language: String) async throws -> [ChunkMinutes] {
        do {
            return [try await model.extract(chunk, language: language)]
        } catch let error as MinutesModelError {
            let halves = TranscriptChunker.halves(of: chunk)
            guard halves.count == 2 else { throw error }
            try Task.checkCancellation()
            return try await extract(halves[0], language: language) + extract(halves[1], language: language)
        }
    }

    /// The overview from every key point; when they do not fit in one call, partial overviews of
    /// groups of points first, then the overview of those.
    private func overview(of points: [String], language: String, budget: Int) async throws -> String {
        guard !points.isEmpty else { return "" }
        let groups = TranscriptChunker.group(points, budget: budget)
        if groups.count == 1 { return try await fittedOverview(of: points, language: language) }
        var partial: [String] = []
        for group in groups {
            try Task.checkCancellation()
            partial.append(try await fittedOverview(of: group, language: language))
        }
        return try await overview(of: partial, language: language, budget: budget)
    }

    /// An overview of points that should fit one call; when the model says they do not, or its
    /// answer is cut off, of each half first, then of the two.
    private func fittedOverview(of points: [String], language: String) async throws -> String {
        do {
            return try await model.overview(of: points, language: language)
        } catch let error as MinutesModelError {
            let parts: [[String]]
            if points.count > 1 {
                let middle = points.count / 2
                parts = [Array(points[..<middle]), Array(points[middle...])]
            } else if let only = points.first, case let halves = TranscriptChunker.halves(of: only), halves.count == 2 {
                parts = halves.map { [$0] }
            } else {
                throw error
            }
            var partial: [String] = []
            for part in parts {
                try Task.checkCancellation()
                partial.append(try await fittedOverview(of: part, language: language))
            }
            return try await model.overview(of: partial, language: language)
        }
    }
}

/// Cutting a transcript into chunks that fit a model's context.
enum TranscriptChunker {
    /// One line per turn (`[mm:ss] Speaker: text`), consecutive segments of the same speaker joined.
    static func turns(_ segments: [TranscriptSegment]) -> [String] {
        var turns: [(speaker: String?, start: Int, text: String)] = []
        for segment in segments {
            let text = segment.text.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty else { continue }
            if let last = turns.last, last.speaker == segment.speaker {
                turns[turns.count - 1].text += " " + text
            } else {
                turns.append((segment.speaker, segment.startMs, text))
            }
        }
        return turns.map { turn in
            let seconds = turn.start / 1000
            let stamp = String(format: "[%02d:%02d]", seconds / 60, seconds % 60)
            return turn.speaker.map { "\(stamp) \($0): \(turn.text)" } ?? "\(stamp) \(turn.text)"
        }
    }

    /// The transcript in chunks of at most `budget` characters, cut only between turns; a turn
    /// longer than the budget is cut between sentences, then between words.
    static func chunks(_ segments: [TranscriptSegment], budget: Int) -> [String] {
        let lines = turns(segments).flatMap { split($0, budget: budget) }
        return group(lines, budget: budget).map { $0.joined(separator: "\n") }
    }

    /// Lines packed in order into groups whose joined length stays within `budget`.
    static func group(_ lines: [String], budget: Int) -> [[String]] {
        var groups: [[String]] = []
        var current: [String] = []
        var length = 0
        for line in lines {
            let added = line.count + (current.isEmpty ? 0 : 1)
            if !current.isEmpty, length + added > budget {
                groups.append(current)
                current = []
                length = 0
            }
            length += line.count + (current.isEmpty ? 0 : 1)
            current.append(line)
        }
        if !current.isEmpty { groups.append(current) }
        return groups
    }

    /// A line cut into pieces of at most `budget` characters, between sentences when it can be.
    static func split(_ line: String, budget: Int) -> [String] {
        guard line.count > budget, budget > 0 else { return [line] }
        var sentences: [String] = []
        line.enumerateSubstrings(in: line.startIndex..., options: .bySentences) { sentence, _, _, _ in
            if let sentence { sentences.append(sentence.trimmingCharacters(in: .whitespaces)) }
        }
        if sentences.isEmpty { sentences = [line] }
        var pieces: [String] = []
        for sentence in sentences {
            if sentence.count <= budget {
                pieces.append(sentence)
            } else {
                // Between words; a word still longer (text without spaces, such as Chinese or
                // Japanese) between characters.
                let words = sentence.split(separator: " ").flatMap { word in
                    word.count <= budget ? [String(word)] : Self.pieces(of: String(word), length: budget)
                }
                pieces += group(words, budget: budget).map { $0.joined(separator: " ") }
            }
        }
        return group(pieces, budget: budget).map { $0.joined(separator: " ") }
    }

    /// A chunk in two halves cut at the line nearest its middle, between words for one line, or
    /// between characters for one word (text without spaces, such as Chinese or Japanese).
    static func halves(of chunk: String) -> [String] {
        let lines = chunk.components(separatedBy: "\n")
        if lines.count > 1 {
            let middle = lines.count / 2
            return [lines[..<middle].joined(separator: "\n"), lines[middle...].joined(separator: "\n")]
        }
        let words = chunk.split(separator: " ")
        if words.count > 1 {
            let middle = words.count / 2
            return [words[..<middle].joined(separator: " "), words[middle...].joined(separator: " ")]
        }
        guard chunk.count > 1 else { return [chunk] }
        let middle = chunk.index(chunk.startIndex, offsetBy: chunk.count / 2)
        return [String(chunk[..<middle]), String(chunk[middle...])]
    }

    /// A text cut every `length` characters.
    static func pieces(of text: String, length: Int) -> [String] {
        guard length > 0 else { return [text] }
        var pieces: [String] = []
        var rest = Substring(text)
        while !rest.isEmpty {
            let end = rest.index(rest.startIndex, offsetBy: length, limitedBy: rest.endIndex) ?? rest.endIndex
            pieces.append(String(rest[..<end]))
            rest = rest[end...]
        }
        return pieces
    }
}

/// How many characters a token takes, estimated conservatively from the text's scripts, for when
/// the model cannot count them: 2.5 for ASCII, 1.67 for other alphabets (accented Latin,
/// Cyrillic, Greek), and 1 for Chinese, Japanese, Korean, Thai and the like.
enum TokenEstimate {
    static func charactersPerToken(_ text: String) -> Double {
        var characters = 0
        var tokens = 0.0
        for scalar in text.unicodeScalars.prefix(50_000) where !scalar.properties.isWhitespace {
            characters += 1
            tokens += cost(of: scalar)
        }
        guard tokens > 0 else { return 2.5 }
        return Double(characters) / tokens
    }

    static func cost(of scalar: Unicode.Scalar) -> Double {
        if scalar.isASCII { return 0.4 }
        switch scalar.value {
        case 0x0000...0x058F: return 0.6 // Latin, Greek, Cyrillic, Armenian
        default: return 1
        }
    }
}

/// Joining the minutes of every chunk: everything kept, in order, with only exact or near-exact
/// repeats merged.
enum MinutesMerger {
    static func merge(_ parts: [ChunkMinutes]) -> ChunkMinutes {
        var result = ChunkMinutes()
        result.keyPoints = dedupe(parts.flatMap(\.keyPoints))
        result.decisions = dedupe(parts.flatMap(\.decisions))
        result.openQuestions = dedupe(parts.flatMap(\.openQuestions))
        var items: [ActionItem] = []
        for item in parts.flatMap(\.actionItems) {
            let task = item.task.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !task.isEmpty else { continue }
            let owner = item.owner?.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty
            if let index = items.firstIndex(where: { isNearDuplicate($0.task, task) && sameOwner($0.owner, owner) }) {
                if items[index].owner == nil { items[index].owner = owner }
            } else {
                items.append(ActionItem(task: task, owner: owner))
            }
        }
        result.actionItems = items
        return result
    }

    static func dedupe(_ items: [String]) -> [String] {
        var kept: [String] = []
        for item in items {
            let trimmed = item.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty, !kept.contains(where: { isNearDuplicate($0, trimmed) }) else { continue }
            kept.append(trimmed)
        }
        return kept
    }

    /// The same once case, accents, punctuation and spacing are ignored, or at most one edit in
    /// twenty characters apart (a typo, a plural), never a different number.
    static func isNearDuplicate(_ lhs: String, _ rhs: String) -> Bool {
        let a = normalized(lhs)
        let b = normalized(rhs)
        if a == b { return true }
        guard digits(a) == digits(b) else { return false }
        let longest = max(a.count, b.count)
        guard longest >= 20, abs(a.count - b.count) <= longest / 20 else { return false }
        return editDistance(Array(a), Array(b), limit: longest / 20) <= longest / 20
    }

    private static func sameOwner(_ lhs: String?, _ rhs: String?) -> Bool {
        guard let lhs, let rhs else { return true }
        return normalized(lhs) == normalized(rhs)
    }

    static func normalized(_ text: String) -> String {
        let folded = text.folding(options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive], locale: nil)
        let scalars = folded.unicodeScalars.map { CharacterSet.alphanumerics.contains($0) ? Character($0) : " " }
        return String(scalars).split(separator: " ").joined(separator: " ")
    }

    private static func digits(_ text: String) -> String {
        String(text.filter(\.isNumber))
    }

    /// Levenshtein distance, giving up once it is above `limit`.
    private static func editDistance(_ a: [Character], _ b: [Character], limit: Int) -> Int {
        guard !a.isEmpty, !b.isEmpty else { return max(a.count, b.count) }
        var previous = Array(0...b.count)
        for i in 1...a.count {
            var current = [i] + Array(repeating: 0, count: b.count)
            var rowMin = i
            for j in 1...b.count {
                let cost = a[i - 1] == b[j - 1] ? 0 : 1
                current[j] = min(previous[j] + 1, current[j - 1] + 1, previous[j - 1] + cost)
                rowMin = min(rowMin, current[j])
            }
            if rowMin > limit { return rowMin }
            previous = current
        }
        return previous[b.count]
    }
}

extension String {
    fileprivate var nilIfEmpty: String? { isEmpty ? nil : self }
}

/// The English name of a transcript's language, for the model's instructions.
enum MinutesLanguage {
    static func name(of identifier: String) -> String {
        let locale = Locale(identifier: identifier)
        let code = locale.language.languageCode?.identifier ?? identifier
        return Locale(identifier: "en").localizedString(forLanguageCode: code) ?? code
    }
}
