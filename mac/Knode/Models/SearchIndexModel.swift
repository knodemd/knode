import Foundation
import Observation

/// Search by meaning: whether it is on (`embeddings` in `config.toml`), the search model, and how
/// far Knode Service has indexed each open space. Shown in Settings and the menu bar.
@MainActor
@Observable
final class SearchIndexModel {
    enum State: Equatable {
        case loading
        case loaded
        /// Knode Service is not running: it does the indexing, and search is by words meanwhile.
        case serviceStopped
    }

    private(set) var status: IndexStatus?
    private(set) var state = State.loading
    /// `embeddings` in `config.toml`, which the app changes.
    private(set) var isEnabled = true
    /// A call is running.
    private(set) var isBusy = false
    var errorMessage: String?

    @ObservationIgnored private let service: any SearchIndexService
    @ObservationIgnored private let settings: any KnodeSettings
    @ObservationIgnored private let startService: @MainActor () async -> Void
    @ObservationIgnored private let pollInterval: Duration

    /// - Parameters:
    ///   - service: where the index is: Knode Service.
    ///   - settings: `config.toml`, where search by meaning is turned on and off.
    ///   - startService: starts Knode Service, as the menu bar does.
    ///   - pollInterval: how often `watch()` asks for the status.
    init(
        service: any SearchIndexService,
        settings: any KnodeSettings,
        startService: @escaping @MainActor () async -> Void = {},
        pollInterval: Duration = .seconds(2)
    ) {
        self.service = service
        self.settings = settings
        self.startService = startService
        self.pollInterval = pollInterval
        isEnabled = (try? settings.embeddingsEnabled()) ?? true
    }

    /// Reads the setting and asks Knode Service how far the index is.
    func load() async {
        refreshEnabled()
        await perform(busy: false) { try await self.service.status() }
    }

    /// Asks for the status again and again while the task runs, such as while Settings shows it.
    func watch() async {
        while !Task.isCancelled {
            await load()
            try? await Task.sleep(for: pollInterval)
        }
    }

    /// Reads `embeddings` in `config.toml` again; someone may have run `knode index off`.
    func refreshEnabled() {
        if let enabled = try? settings.embeddingsEnabled(), enabled != isEnabled { isEnabled = enabled }
    }

    /// Turns search by meaning on or off. On, Knode Service downloads its model the first time
    /// and indexes the open spaces; off, it downloads and indexes nothing, and search is by words.
    func setEnabled(_ on: Bool) async {
        do {
            try settings.setEmbeddingsEnabled(on)
            isEnabled = on
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
            return
        }
        // The service reads the setting again when asked for its status.
        await perform { try await self.service.status() }
    }

    /// Indexes every open space now, even on battery, and tries a failed download again.
    func indexNow() async {
        await perform { try await self.service.run() }
    }

    /// Starts Knode Service, then asks it for the status.
    func startServiceAndLoad() async {
        isBusy = true
        await startService()
        isBusy = false
        await load()
    }

    /// Runs a call and keeps the status it answers. With `busy`, the actions wait meanwhile.
    private func perform(busy: Bool = true, _ work: @escaping () async throws -> IndexStatus) async {
        if busy { isBusy = true }
        defer { if busy { isBusy = false } }
        do {
            let new = try await work()
            // Set only what changed, so a status asked for every few seconds redraws nothing.
            if status != new { status = new }
            if state != .loaded { state = .loaded }
            if errorMessage != nil { errorMessage = nil }
        } catch let error as ServiceError where error.meansNotRunning {
            if status != nil { status = nil }
            if state != .serviceStopped { state = .serviceStopped }
        } catch {
            if state != .loaded { state = .loaded }
            errorMessage = error.localizedDescription
        }
    }
}
