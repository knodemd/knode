import Foundation

/// Runs the Photos library source for every space it is connected to: Knode Service lists it and
/// keeps its settings and state, but only the app can read PhotoKit (`PROTOCOL.md`, "Photos
/// library").
///
/// A run, for each space: ask its plan (`photos_library.plan`: on, the albums picked on this Mac,
/// From today, iCloud, room), list the assets of those albums, ask which the space has or does not
/// want (`photos_library.known`: held, deleted by the person, or failed for good), export each new
/// one (a photo's original unchanged; a video, keeping only its audio when it is larger than a
/// space attaches) and hand it to `photos_library.add`, then end with `photos_library.sync`,
/// which marks the assets that left the albums, but only when every album picked was found.
///
/// A space is read again when the service asks (its `next_run` came, or the person chose Run
/// now), when the library changed (its change token differs from the one saved by the last run),
/// or when the albums picked changed. Once a space was read in full since the app started, a run
/// with a valid saved token reads only what changed since it (`changes(since:)`): nothing when no
/// asset or album picked changed, else only the assets that changed, those of an album that
/// changed, and those to try again. Listing never reads an asset's resources; only exporting
/// does. Originals kept only in iCloud wait unless the space allows downloading them; nothing new
/// is added once the space is past its size limit. Without access, each space is told why once
/// (`photos_library.fail`). The scratch folder is emptied when the syncer starts, so exports a
/// crash left behind go.
actor PhotosLibrarySyncer {
    private let library: any PhotoLibrary
    private let client: any PhotosLibraryClient
    private let extractor: any AudioExtractor
    private let limit: Int64
    private let scratch: URL
    private let now: @Sendable () -> Date
    /// The albums each space read last, to read it again when they change.
    private var lastAlbums: [URL: [String]] = [:]
    /// The spaces read in full since the app started: only these are read by changes.
    private var scanned: Set<URL> = []
    /// The assets of each space to try again: they waited for iCloud or for room, or failed
    /// for a reason that may pass.
    private var retry: [URL: Set<String>] = [:]
    private var cleared = false
    private var libraryChanged = true
    private var deniedReported: Set<URL> = []
    private var loop: Task<Void, Never>?
    private var observer: Task<Void, Never>?

    init(
        library: any PhotoLibrary, client: any PhotosLibraryClient, extractor: any AudioExtractor = AACAudioExtractor(),
        limit: Int64 = AttachmentInfo.maxSize,
        scratch: URL = FileManager.default.temporaryDirectory.appending(path: "Knode Photos Library", directoryHint: .isDirectory),
        now: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.library = library
        self.client = client
        self.extractor = extractor
        self.limit = limit
        self.scratch = scratch
        self.now = now
    }

    /// Says the library changed: the next run reads every space again.
    func libraryDidChange() { libraryChanged = true }

    /// Runs every `interval`, and soon after the library changes, until `stop()`.
    func start(every interval: Duration = .seconds(60)) {
        guard loop == nil else { return }
        loop = Task { [weak self] in
            while !Task.isCancelled {
                await self?.observeChanges()
                await self?.runOnce()
                try? await Task.sleep(for: interval)
            }
        }
    }

    func stop() {
        loop?.cancel()
        loop = nil
        observer?.cancel()
        observer = nil
    }

    private func observeChanges() {
        guard observer == nil, library.access == .authorized else { return }
        let changes = library.changes()
        observer = Task { [weak self] in
            for await _ in changes { await self?.libraryDidChange() }
        }
    }

    /// Empties the scratch folder once: exports are deleted after each add, so anything there was
    /// left by a crash.
    private func clearScratch() {
        guard !cleared else { return }
        cleared = true
        try? FileManager.default.removeItem(at: scratch)
    }

    /// One run over every space the source is connected to. Errors of one space never stop the
    /// others; a service that is not running ends the run quietly.
    func runOnce() async {
        clearScratch()
        let spaces: [URL]
        do {
            spaces = try await client.connectedSpaces()
        } catch {
            return
        }
        lastAlbums = lastAlbums.filter { spaces.contains($0.key) }
        scanned = scanned.filter { spaces.contains($0) }
        retry = retry.filter { spaces.contains($0.key) }
        guard !spaces.isEmpty else { return }
        guard library.access == .authorized else {
            for space in spaces where !deniedReported.contains(space) {
                let message = library.access == .restricted
                    ? "Photos access is restricted on this Mac"
                    : "Knode may not read your Photos library: allow it in System Settings > Privacy & Security > Photos"
                if (try? await client.fail(kind: "permission", message: message, in: space)) != nil {
                    deniedReported.insert(space)
                }
            }
            return
        }
        deniedReported = []
        let albums = library.albums()
        try? await client.reportAlbums(albums)
        let changed = libraryChanged
        libraryChanged = false
        let token = library.changeToken()
        var failed = false
        for space in spaces {
            do {
                try await run(space, albums: albums, token: token, libraryChanged: changed)
            } catch {
                failed = true
            }
        }
        if failed { libraryChanged = true }
    }

    private func run(_ space: URL, albums: [PhotoAlbum], token: String?, libraryChanged: Bool) async throws {
        let plan = try await client.plan(in: space)
        guard plan.on else { return }
        let picked = plan.albums.map(\.id)
        let asked = plan.nextRun.map { $0 <= RecordingWire.milliseconds(now()) } ?? true
        let albumsChanged = lastAlbums[space] != picked
        let tokenChanged = token == nil || token != plan.token
        guard asked || albumsChanged || (libraryChanged && tokenChanged) else { return }
        let again = retry[space] ?? []
        // What changed since the last run, when it can be told: else everything is read.
        var changes: PhotoLibraryChanges?
        if !albumsChanged, scanned.contains(space), let saved = plan.token {
            changes = saved == token ? PhotoLibraryChanges() : library.changes(since: saved)
        }
        let albumChanged = changes.map { !$0.albums.isDisjoint(with: picked) } ?? true
        if let changes, changes.assets.isEmpty, !albumChanged, again.isEmpty {
            try await client.sync(present: nil, failed: [], albums: albums, token: token, added: 0, waiting: 0, held: 0, in: space)
            lastAlbums[space] = picked
            return
        }
        let contents = library.assets(inAlbums: Set(picked))
        // An album picked that is not found (deleted, or of another library) says nothing about
        // what left: nothing is marked.
        let present = contents.found == Set(picked) ? contents.assets.map(\.id) : nil
        let candidates = contents.assets.filter { asset in
            guard let changes, !albumChanged else { return true }
            return changes.assets.contains(asset.id) || again.contains(asset.id)
        }
        let wanted = candidates.filter { asset in
            guard let since = plan.since, let created = asset.createdAt else { return true }
            return RecordingWire.milliseconds(created) >= since
        }
        let known = wanted.isEmpty ? [] : try await client.known(wanted.map(\.id), in: space)
        let new = wanted.filter { !known.contains($0.id) }
        var added = 0, waiting = 0, held = 0
        var failures: [PhotosLibraryFailure] = []
        var later = Set<String>()
        var room = plan.room
        for (index, asset) in new.enumerated() {
            guard room else {
                held = new.count - index
                later.formUnion(new[index...].map(\.id))
                break
            }
            do {
                let result = try await add(asset, allowNetwork: plan.icloud, in: space)
                if result.created { added += 1 }
                room = result.room
            } catch PhotoExportError.inCloud {
                waiting += 1
                later.insert(asset.id)
            } catch let error where Self.isPermanent(error) {
                // Never addable: the core remembers it, and it is not exported again.
                failures.append(PhotosLibraryFailure(id: asset.id, reason: error.localizedDescription))
            } catch {
                // One asset that cannot be exported or added does not stop the others; the next
                // run tries it again.
                later.insert(asset.id)
            }
        }
        try await client.sync(present: present, failed: failures, albums: albums, token: token, added: added, waiting: waiting, held: held, in: space)
        lastAlbums[space] = picked
        scanned.insert(space)
        retry[space] = later
    }

    /// Whether an asset failed for good: its audio alone is too large, or it has no original.
    private static func isPermanent(_ error: Error) -> Bool {
        switch error {
        case MediaImportError.audioTooLong, MediaImportError.tooLarge, PhotoExportError.noOriginal: true
        default: false
        }
    }

    private func add(_ asset: PhotoAsset, allowNetwork: Bool, in space: URL) async throws -> PhotosLibraryAdded {
        let folder = scratch.appending(path: UUID().uuidString, directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: folder) }
        let exported = try await library.exportOriginal(of: asset, into: folder, allowNetwork: allowNetwork)
        let file = exported.file
        var export = PhotosLibraryExport(asset: asset, file: file, metadata: PhotoMetadataDraft(), originalName: file.lastPathComponent,
                                         original: nil, content: exported.content)
        switch asset.kind {
        case .photo:
            var metadata = PhotoMetadataReader.read(file) ?? PhotoMetadataDraft()
            metadata.takenAt = metadata.takenAt ?? asset.createdAt
            metadata.latitude = metadata.latitude ?? asset.latitude
            metadata.longitude = metadata.longitude ?? asset.longitude
            metadata.width = metadata.width ?? asset.width
            metadata.height = metadata.height ?? asset.height
            export.metadata = metadata
        case .video:
            if Self.size(of: file) > limit {
                // PhotoKit can export only the audio, but the whole original is read here anyway
                // for its hash: it is deleted as soon as its audio is out.
                let stem = file.deletingPathExtension().lastPathComponent
                let audio = try await extractor.extractAudio(from: file, named: stem, into: folder)
                try? FileManager.default.removeItem(at: file)
                guard Self.size(of: audio) <= limit else {
                    throw MediaImportError.audioTooLong(file.lastPathComponent, limit: limit)
                }
                export.file = audio
                export.original = file.lastPathComponent
            }
        }
        return try await client.add(export, in: space)
    }

    private static func size(of url: URL) -> Int64 {
        Int64((try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0)
    }
}

/// Connecting the Photos library asks for Photos access, as the sign-in step of the Connect
/// sheet: never before. Once allowed, the albums of the Mac are reported to Knode Service, so the
/// sheet can list them to pick from.
struct PhotosLibraryAccessSignIn: SourceSignIn {
    enum Failure: LocalizedError {
        case denied

        var errorDescription: String? {
            "Knode may not read your Photos library. Allow it in System Settings > Privacy & Security > Photos, then check again."
        }
    }

    let library: any PhotoLibrary
    let client: any PhotosLibraryClient
    var openURL: @Sendable (URL) -> Void = { _ in }

    /// Privacy & Security > Photos in System Settings.
    static let settingsURL = URL(string: "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Photos")!

    func start(source: String, space: URL) async throws -> SourceSignInStep? {
        switch library.access {
        case .authorized:
            try await report()
            return nil
        case .notDetermined:
            return SourceSignInStep(
                id: "photos-access", title: "Allow Photos access",
                message: "Knode reads only the albums you pick, on this Mac, and keeps each photo and video in this space with what it finds in it. It never changes your library and sends nothing anywhere.",
                continueTitle: "Allow Access"
            )
        case .denied, .restricted:
            return deniedStep
        }
    }

    func answer(_ step: SourceSignInStep, with values: [String: String], source: String, space: URL) async throws -> SourceSignInStep? {
        if library.access == .notDetermined {
            _ = await library.requestAccess()
        }
        guard library.access == .authorized else {
            if step.id == "photos-denied" { openURL(Self.settingsURL) }
            throw Failure.denied
        }
        try await report()
        return nil
    }

    private var deniedStep: SourceSignInStep {
        SourceSignInStep(
            id: "photos-denied", title: "Photos access is off",
            message: "Allow Knode in System Settings > Privacy & Security > Photos, then check again.",
            continueTitle: "Check Again"
        )
    }

    private func report() async throws {
        try await client.reportAlbums(library.albums())
    }
}
