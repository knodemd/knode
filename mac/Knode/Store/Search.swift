import Foundation

/// How a search finds notes: by the words they contain, by what they mean, or both ranked
/// together. Search by meaning runs in Knode Service, on this machine.
public enum SearchMode: String, CaseIterable, Identifiable, Sendable {
    case hybrid, words, meaning

    public var id: Self { self }

    public var title: String {
        switch self {
        case .hybrid: "Words and Meaning"
        case .words: "Words"
        case .meaning: "Meaning"
        }
    }

    public var systemImage: String {
        switch self {
        case .hybrid: "text.magnifyingglass"
        case .words: "textformat"
        case .meaning: "sparkle.magnifyingglass"
        }
    }

    /// What the mode finds, in a sentence.
    public var explanation: String {
        switch self {
        case .hybrid: "Notes with the words you type, and notes about the same thing in other words."
        case .words: "Only notes with the words you type."
        case .meaning: "Notes about what you type, even when they use other words."
        }
    }
}

/// One note found by a search.
public struct SearchHit: Identifiable, Hashable, Sendable {
    public var summary: NodeSummary
    /// The text around the words found, with each match in `**bold**`, or the start of the
    /// passage nearest in meaning.
    public var snippet: String
    /// Its place among the notes found by words, from 1, or `nil` when words did not find it.
    public var wordRank: Int?
    /// Its place among the notes found by meaning, from 1, or `nil` when meaning did not find it.
    public var meaningRank: Int?

    public var id: Node.ID { summary.id }

    public init(summary: NodeSummary, snippet: String, wordRank: Int?, meaningRank: Int?) {
        self.summary = summary
        self.snippet = snippet
        self.wordRank = wordRank
        self.meaningRank = meaningRank
    }

    /// The snippet split into plain and bold runs, at each `**`.
    public var snippetRuns: [(text: String, bold: Bool)] {
        snippet.components(separatedBy: "**").enumerated().compactMap { index, text in
            text.isEmpty ? nil : (text, index % 2 == 1)
        }
    }
}

/// The notes a search found, and how it ran.
public struct SearchResults: Hashable, Sendable {
    /// The mode that ran: `words` when meaning was asked for but cannot be used yet.
    public var mode: SearchMode
    /// Best first.
    public var hits: [SearchHit]
    /// Why the search ran by words, or that some notes were found by words only; `nil` when
    /// there is nothing to say.
    public var note: String?

    public init(mode: SearchMode, hits: [SearchHit], note: String?) {
        self.mode = mode
        self.hits = hits
        self.note = note
    }

    /// Results found by words alone, ranked as given.
    public static func words(_ summaries: [NodeSummary], snippets: [String]? = nil, note: String?) -> SearchResults {
        let hits = summaries.enumerated().map { index, summary in
            SearchHit(summary: summary, snippet: snippets?[index] ?? summary.excerpt, wordRank: index + 1, meaningRank: nil)
        }
        return SearchResults(mode: .words, hits: hits, note: note)
    }

    /// Why a store that cannot search by meaning searched by words.
    static let noServiceNote = "Knode Service is not running, so this search is by words. Search by meaning runs in Knode Service: start it from the menu bar."
}

extension SpaceStore {
    /// A store that only searches by words runs every mode by words, and says so.
    public func search(_ query: String, mode: SearchMode, in space: Space.ID) async throws -> SearchResults {
        let found = try await search(query, in: space)
        return .words(found, note: mode == .words ? nil : "Search by meaning is not available here, so this search is by words.")
    }
}

// MARK: - Index

/// How far Knode Service has indexed the open spaces to search them by meaning: `index.status`,
/// and `index` in `service.status`.
public struct IndexStatus: Decodable, Equatable, Sendable {
    public struct Model: Decodable, Equatable, Sendable {
        public enum State: String, Decodable, Sendable {
            case unavailable, missing, downloading, downloaded, loading, ready, failed
        }

        /// Such as `multilingual-e5-small`; empty when this build has none.
        public var name: String
        public var state: State
        /// Bytes downloaded so far, while it downloads.
        public var downloaded: Int64
        /// The size of its files, in bytes.
        public var size: Int64
        public var error: String?

        public init(name: String, state: State, downloaded: Int64 = 0, size: Int64 = 0, error: String? = nil) {
            self.name = name
            self.state = state
            self.downloaded = downloaded
            self.size = size
            self.error = error
        }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            name = try container.decodeIfPresent(String.self, forKey: .name) ?? ""
            // A state a newer service adds reads as not available.
            state = (try? container.decode(State.self, forKey: .state)) ?? .unavailable
            downloaded = try container.decodeIfPresent(Int64.self, forKey: .downloaded) ?? 0
            size = try container.decodeIfPresent(Int64.self, forKey: .size) ?? 0
            error = try container.decodeIfPresent(String.self, forKey: .error)
        }

        private enum CodingKeys: String, CodingKey { case name, state, downloaded, size, error }

        /// How much of it is downloaded, from 0 to 1, while it downloads.
        public var progress: Double? {
            guard state == .downloading, size > 0 else { return nil }
            return min(1, Double(downloaded) / Double(size))
        }

        /// Where the model is, in a few words.
        public var summary: String {
            let bytes = ByteCountFormatStyle(style: .file)
            switch state {
            case .unavailable: return "Not available"
            case .missing: return size > 0 ? "Not downloaded yet (\(size.formatted(bytes)))" : "Not downloaded yet"
            case .downloading:
                let percent = Int((progress ?? 0) * 100)
                return "Downloading \(percent)% (\(downloaded.formatted(bytes)) of \(size.formatted(bytes)))"
            case .downloaded: return "Downloaded"
            case .loading: return "Loading"
            case .ready: return "Ready"
            case .failed: return "Download failed: \(error ?? "unknown error"). Knode tries again in a few minutes."
            }
        }
    }

    /// How far one open space is indexed.
    public struct SpaceIndex: Decodable, Equatable, Sendable, Identifiable {
        /// The path of the space, as Knode Service opened it.
        public var space: String
        public var nodes: Int
        /// The notes that wait to be indexed: new, changed, or not indexed yet.
        public var waiting: Int
        /// Whether it is being indexed now, or will be soon.
        public var indexing: Bool
        /// What went wrong the last time it was indexed.
        public var error: String?

        public var id: String { space }

        public init(space: String, nodes: Int, waiting: Int, indexing: Bool, error: String? = nil) {
            self.space = space
            self.nodes = nodes
            self.waiting = waiting
            self.indexing = indexing
            self.error = error
        }

        public var name: String {
            URL(filePath: space).deletingPathExtension().lastPathComponent
        }

        public var indexed: Int { max(0, nodes - waiting) }

        /// How far it is, in a few words.
        public var summary: String {
            if let error { return "Error: \(error)" }
            let notes = nodes == 1 ? "1 note" : "\(nodes.formatted()) notes"
            if waiting > 0 { return "\(indexed.formatted()) of \(notes) indexed, \(waiting.formatted()) waiting" }
            if indexing { return "\(notes), indexing" }
            return "\(notes), all indexed"
        }
    }

    /// Whether this build of Knode can search by meaning.
    public var available: Bool
    /// `embeddings` in `config.toml`.
    public var enabled: Bool
    /// Whether the service indexes at all.
    public var running: Bool
    public var model: Model
    /// Why indexing waits, such as `on battery`.
    public var paused: String?
    public var spaces: [SpaceIndex]

    public init(available: Bool, enabled: Bool, running: Bool, model: Model, paused: String? = nil, spaces: [SpaceIndex] = []) {
        self.available = available
        self.enabled = enabled
        self.running = running
        self.model = model
        self.paused = paused
        self.spaces = spaces
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        available = try container.decode(Bool.self, forKey: .available)
        enabled = try container.decode(Bool.self, forKey: .enabled)
        running = try container.decodeIfPresent(Bool.self, forKey: .running) ?? true
        model = try container.decode(Model.self, forKey: .model)
        paused = try container.decodeIfPresent(String.self, forKey: .paused)
        spaces = try container.decodeIfPresent([SpaceIndex].self, forKey: .spaces) ?? []
    }

    private enum CodingKeys: String, CodingKey { case available, enabled, running, model, paused, spaces }

    /// Whether search by meaning is on and this service indexes.
    public var isActive: Bool { available && enabled && running }

    /// The state of search by meaning as a whole, in a sentence.
    public var summary: String {
        if !available { return "This build of Knode Service has no search by meaning, so search is by words." }
        if !enabled { return "Search by meaning is off, so search is by words." }
        if !running { return "Search by meaning is on, but this Knode Service does not index." }
        if let paused { return "Indexing is paused: \(paused)." }
        if model.state == .downloading || model.state == .missing { return "Getting the search model ready." }
        if model.state == .failed { return "The search model could not be downloaded." }
        let waiting = spaces.reduce(0) { $0 + $1.waiting }
        if waiting > 0 { return waiting == 1 ? "Indexing: 1 note left." : "Indexing: \(waiting.formatted()) notes left." }
        return "Search by meaning is ready."
    }
}

/// Indexing for search by meaning, in Knode Service. Only the person may ask.
public protocol SearchIndexService: Sendable {
    /// `index.status`: the model and how far each open space is indexed. The service reads
    /// `embeddings` in `config.toml` again first.
    func status() async throws -> IndexStatus
    /// `index.run`: indexes every open space now, even on battery, and tries a failed download
    /// again.
    func run() async throws -> IndexStatus
}

/// The `SearchIndexService` of Knode Service, over its socket as the person.
public struct ServiceSearchIndex: SearchIndexService {
    public var socketURL: URL

    public init(socketURL: URL = KnodeHome.current.socketURL) {
        self.socketURL = socketURL
    }

    public func status() async throws -> IndexStatus {
        try await call("index.status")
    }

    public func run() async throws -> IndexStatus {
        try await call("index.run")
    }

    /// One short connection per call, like the Agents settings.
    private func call(_ method: String) async throws -> IndexStatus {
        let connection = try ServiceConnection.connect(to: socketURL)
        defer { connection.close() }
        return try await connection.call(method, as: IndexStatus.self)
    }
}
