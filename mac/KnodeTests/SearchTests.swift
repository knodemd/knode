import Foundation
import Testing
@testable import Knode

/// Search by words and meaning, and the index status, against a fake Knode Service; and search
/// by words without it.
@Suite(.serialized) final class SearchTests {
    let service: FakeKnodeService
    let path = "/tmp/Search.knode"
    let folder: URL

    init() throws {
        service = try FakeKnodeService()
        folder = FileManager.default.temporaryDirectory.appending(path: "KnodeSearchTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    }

    deinit {
        service.stop()
        try? FileManager.default.removeItem(at: folder)
    }

    var url: URL { URL(filePath: path, directoryHint: .isDirectory) }

    // MARK: Store

    @Test func searchesInTheModeAskedForWithRanksAndSnippets() async throws {
        let store = ServiceSpaceStore(socketURL: service.socketURL, person: "Ada")
        let space = try await store.openSpace(at: url)
        _ = try await store.createNode(in: space.id, title: "Garden plan", body: "Tomatoes in May.")
        _ = try await store.createNode(in: space.id, title: "Plan for the trip", body: "Train to Lisbon.")

        let results = try await store.search("plan", mode: .hybrid, in: space.id)
        #expect(service.params(of: "search")?["mode"] as? String == "hybrid")
        #expect(results.mode == .hybrid)
        #expect(results.note == nil)
        #expect(results.hits.map(\.summary.title) == ["Garden plan", "Plan for the trip"])
        #expect(results.hits.map(\.wordRank) == [1, 2])
        #expect(results.hits.map(\.meaningRank) == [2, 1])
        let first = try #require(results.hits.first)
        #expect(first.snippet == "about **Garden plan**")
        #expect(first.summary.excerpt == "about Garden plan")
        #expect(first.snippetRuns.map(\.text) == ["about ", "Garden plan"])
        #expect(first.snippetRuns.map(\.bold) == [false, true])

        let words = try await store.search("plan", mode: .words, in: space.id)
        #expect(words.mode == .words)
        #expect(words.hits.allSatisfy { $0.meaningRank == nil })
        let meaning = try await store.search("plan", mode: .meaning, in: space.id)
        #expect(meaning.hits.allSatisfy { $0.wordRank == nil })
    }

    @Test func saysWhyMeaningFellBackToWords() async throws {
        let store = ServiceSpaceStore(socketURL: service.socketURL, person: "Ada")
        let space = try await store.openSpace(at: url)
        _ = try await store.createNode(in: space.id, title: "Plan", body: "")
        service.setSearchFallback(mode: "words", note: "Search by meaning is getting ready: 120 notes left.")

        let results = try await store.search("plan", mode: .meaning, in: space.id)
        #expect(results.mode == .words)
        #expect(results.note == "Search by meaning is getting ready: 120 notes left.")
        #expect(results.hits.first?.wordRank == 1)
        #expect(results.hits.first?.meaningRank == nil)
    }

    @Test func withoutTheServiceSearchesByWordsAndSaysWhy() async throws {
        let stopped = URL(filePath: "/tmp/kn-none-\(UUID().uuidString.prefix(8))/service.sock")
        let store = RoutingSpaceStore(service: ServiceSpaceStore(socketURL: stopped, person: "Ada"), core: CoreSpaceStore(person: "Ada"))
        let space = try await store.createSpace(at: folder.appending(path: "Words.knode", directoryHint: .isDirectory), name: "Words")
        _ = try await store.createNode(in: space.id, title: "Garden", body: "Plant tomatoes in May.")

        let results = try await store.search("tomatoes", mode: .hybrid, in: space.id)
        #expect(results.mode == .words)
        #expect(results.note == SearchResults.noServiceNote)
        #expect(results.hits.map(\.summary.title) == ["Garden"])
        #expect(results.hits.first?.wordRank == 1)
        #expect(results.hits.first?.snippet.contains("**") == true)

        let words = try await store.search("tomatoes", mode: .words, in: space.id)
        #expect(words.note == nil)
    }

    @Test func theExampleSpaceSearchesByWords() async throws {
        let store = PreviewSpaceStore(seedExamples: true)
        let space = try await store.openSpace(at: SpacePanels.exampleURL)
        let results = try await store.search("search", mode: .meaning, in: space.id)
        #expect(results.mode == .words)
        #expect(results.note?.contains("example space") == true)
        #expect(!results.hits.isEmpty)
    }

    // MARK: App

    @MainActor
    @Test func theListShowsResultsAndTheNote() async throws {
        let store = ServiceSpaceStore(socketURL: service.socketURL, person: "Ada")
        let suite = "md.knode.tests.\(UUID().uuidString)"
        let preferences = try #require(UserDefaults(suiteName: suite))
        defer { preferences.removePersistentDomain(forName: suite) }
        let model = AppModel(store: store, preferences: preferences, searchDelay: .zero, saveDelay: .seconds(60))
        #expect(model.searchMode == .hybrid)
        await model.openSpace(at: url)
        service.agentCreatesNode(in: path, title: "Plan", body: "Ship it.")
        service.agentCreatesNode(in: path, title: "Other", body: "Nothing.")
        #expect(try await eventually { model.nodes.count == 2 })

        model.searchText = "plan"
        #expect(try await eventually { model.searchResults?.hits.map(\.summary.title) == ["Plan"] })
        #expect(model.nodes.map(\.title) == ["Plan"])
        #expect(model.searchResults?.mode == .hybrid)
        #expect(model.searchResults?.note == nil)

        service.setSearchFallback(mode: "words", note: "Search by meaning is getting ready: 120 notes left.")
        model.searchMode = .meaning
        #expect(try await eventually { model.searchResults?.note == "Search by meaning is getting ready: 120 notes left." })
        #expect(service.params(of: "search")?["mode"] as? String == "meaning")
        #expect(model.searchResults?.mode == .words)
        #expect(preferences.string(forKey: AppModel.searchModeKey) == "meaning")

        // The mode is remembered.
        let again = AppModel(store: store, preferences: preferences, searchDelay: .zero, saveDelay: .seconds(60))
        #expect(again.searchMode == .meaning)

        model.searchText = ""
        #expect(try await eventually { model.searchResults == nil && model.nodes.count == 2 })
    }

    // MARK: Index

    @Test func statusDecodesTheIndex() async throws {
        service.setIndex(FakeKnodeService.indexStatus(
            state: "downloading",
            downloaded: 121_931_247,
            spaces: [["space": "/Users/ada/Knode/Work.knode", "nodes": 1200, "waiting": 300, "indexing": true]],
            paused: "on battery"
        ))
        let connection = try ServiceConnection.connect(to: service.socketURL)
        defer { connection.close() }
        let status = try await connection.call("service.status", as: ServiceMonitor.Status.self)
        let index = try #require(status.index)
        #expect(index.isActive)
        #expect(index.model.state == .downloading)
        #expect(abs((index.model.progress ?? 0) - 0.25) < 0.001)
        #expect(index.model.summary.hasPrefix("Downloading 25%"))
        #expect(index.paused == "on battery")
        #expect(index.summary == "Indexing is paused: on battery.")
        let work = try #require(index.spaces.first)
        #expect(work.name == "Work")
        #expect(work.indexed == 900)
        #expect(work.summary == "900 of \(1200.formatted()) notes indexed, 300 waiting")
    }

    @Test func statusSummaries() {
        let model = IndexStatus.Model(name: "multilingual-e5-small", state: .ready)
        #expect(IndexStatus(available: false, enabled: true, running: true, model: model).summary.contains("no search by meaning"))
        #expect(IndexStatus(available: true, enabled: false, running: true, model: model).summary == "Search by meaning is off, so search is by words.")
        let space = IndexStatus.SpaceIndex(space: "/k/Work.knode", nodes: 3, waiting: 1, indexing: true)
        #expect(IndexStatus(available: true, enabled: true, running: true, model: model, spaces: [space]).summary == "Indexing: 1 note left.")
        let done = IndexStatus.SpaceIndex(space: "/k/Work.knode", nodes: 1, waiting: 0, indexing: false)
        #expect(done.summary == "1 note, all indexed")
        #expect(IndexStatus(available: true, enabled: true, running: true, model: model, spaces: [done]).summary == "Search by meaning is ready.")
        #expect(IndexStatus.SpaceIndex(space: "/k/A.knode", nodes: 2, waiting: 0, indexing: false, error: "disk full").summary == "Error: disk full")
    }

    @MainActor
    @Test func settingsTurnSearchByMeaningOffAndIndexNow() async throws {
        let settings = MemoryKnodeSettings()
        service.setIndex(FakeKnodeService.indexStatus(spaces: [["space": path, "nodes": 4, "waiting": 0, "indexing": false]]))
        let index = SearchIndexModel(service: ServiceSearchIndex(socketURL: service.socketURL), settings: settings)
        #expect(index.isEnabled)
        await index.load()
        #expect(index.state == .loaded)
        #expect(index.status?.spaces.first?.summary == "4 notes, all indexed")

        service.setIndex(FakeKnodeService.indexStatus(enabled: false))
        await index.setEnabled(false)
        #expect(try settings.embeddingsEnabled() == false)
        #expect(!index.isEnabled)
        #expect(service.requests.last == "index.status")
        #expect(index.status?.enabled == false)

        await index.indexNow()
        #expect(service.requests.last == "index.run")
        #expect(!index.isBusy)

        // Someone ran `knode index run`, which turns it on again.
        try settings.setEmbeddingsEnabled(true)
        await index.load()
        #expect(index.isEnabled)
    }

    @MainActor
    @Test func settingsSayWhenTheServiceIsStopped() async throws {
        let stopped = URL(filePath: "/tmp/kn-none-\(UUID().uuidString.prefix(8))/service.sock")
        let index = SearchIndexModel(service: ServiceSearchIndex(socketURL: stopped), settings: MemoryKnodeSettings())
        await index.load()
        #expect(index.state == .serviceStopped)
        #expect(index.status == nil)
        // The setting is a file: it changes without the service.
        await index.setEnabled(false)
        #expect(!index.isEnabled)
        #expect(index.errorMessage == nil)
    }
}
