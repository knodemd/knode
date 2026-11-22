import Foundation
import FoundationModels

/// Minutes written by Apple's on-device model (Foundation Models), by map-reduce over the
/// transcript. Nothing leaves the Mac.
typealias FoundationModelsSummarizer = MapReduceSummarizer<FoundationModelsMinutesModel>

extension MapReduceSummarizer where Model == FoundationModelsMinutesModel {
    init() { self.init(model: FoundationModelsMinutesModel()) }
}

@Generable
struct GeneratedActionItem {
    @Guide(description: "The task, with its specifics: what, for when, numbers and names as said.")
    var task: String
    @Guide(description: "The person who takes it on, by the name used in the meeting, or nil when nobody is named.")
    var owner: String?
}

@Generable
struct GeneratedMinutes {
    @Guide(description: "Every point discussed, one sentence each, keeping names, numbers, amounts, dates and places exactly as said.")
    var keyPoints: [String]
    @Guide(description: "Every decision taken, as stated. Empty when none.")
    var decisions: [String]
    @Guide(description: "Every task someone agreed to do. Empty when none.")
    var actionItems: [GeneratedActionItem]
    @Guide(description: "Questions raised and left unanswered. Empty when none.")
    var openQuestions: [String]
}

/// `SystemLanguageModel.default`, read one chunk at a time.
struct FoundationModelsMinutesModel: MinutesModel {
    let name = "Apple Foundation Models (on-device)"

    /// Characters of transcript per call: what is left of the context after the instructions, the
    /// schema and the answer, at a conservative 2.5 characters a token.
    var inputBudget: Int {
        let tokens = max(SystemLanguageModel.default.contextSize - Self.reservedTokens, 800)
        return Int(Double(tokens) * 2.5)
    }

    private static let reservedTokens = 1_700
    private static let responseTokens = 1_000

    func availability(language: String) async -> SummarizerAvailability {
        let model = SystemLanguageModel.default
        switch model.availability {
        case .available:
            guard model.supportsLocale(Locale(identifier: language)) else {
                return .unavailable(.unsupportedLanguage(MinutesLanguage.name(of: language)))
            }
            return .available
        case .unavailable(.appleIntelligenceNotEnabled):
            return .unavailable(.appleIntelligenceNotEnabled)
        case .unavailable(.deviceNotEligible):
            return .unavailable(.deviceNotEligible)
        case .unavailable(.modelNotReady):
            return .unavailable(.modelNotReady)
        case .unavailable(let reason):
            return .unavailable(.other(String(describing: reason)))
        }
    }

    func extract(_ chunk: String, language: String) async throws -> ChunkMinutes {
        let languageName = MinutesLanguage.name(of: language)
        let session = LanguageModelSession(instructions: """
            You take the minutes of a meeting from part of its transcript. Each line is a turn: \
            [minutes:seconds] speaker: words. "Me" is the person taking the minutes; "Others" is \
            everyone on the call. Write in \(languageName), the language of the transcript. \
            Do not leave anything out: keep every name, number, amount, date, deadline and \
            decision exactly as said. Do not invent anything that is not in the transcript.
            """)
        let response = try await respond {
            try await session.respond(
                to: "Transcript:\n\(chunk)",
                generating: GeneratedMinutes.self,
                options: GenerationOptions(samplingMode: .greedy, maximumResponseTokens: Self.responseTokens)
            )
        }
        let minutes = response.content
        return ChunkMinutes(
            keyPoints: minutes.keyPoints,
            decisions: minutes.decisions,
            actionItems: minutes.actionItems.map { ActionItem(task: $0.task, owner: $0.owner) },
            openQuestions: minutes.openQuestions
        )
    }

    func overview(of keyPoints: [String], language: String) async throws -> String {
        let languageName = MinutesLanguage.name(of: language)
        let session = LanguageModelSession(instructions: """
            You write the overview of a meeting's minutes from its key points: one or two short \
            paragraphs in \(languageName) that say what the meeting was about and what came out \
            of it. Keep the names, numbers and dates that matter. Use only what the points say.
            """)
        let points = keyPoints.map { "- \($0)" }.joined(separator: "\n")
        let response = try await respond {
            try await session.respond(
                to: "Key points:\n\(points)",
                options: GenerationOptions(samplingMode: .greedy, maximumResponseTokens: Self.responseTokens)
            )
        }
        return response.content.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Runs a request, reporting a context overflow as `MinutesModelError.contextTooLarge`.
    private func respond<T>(_ request: () async throws -> T) async throws -> T {
        do {
            return try await request()
        } catch LanguageModelSession.GenerationError.exceededContextWindowSize {
            throw MinutesModelError.contextTooLarge
        }
    }
}
