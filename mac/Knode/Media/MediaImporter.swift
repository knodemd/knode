import AVFoundation
import CryptoKit
import Foundation
import Observation
import UniformTypeIdentifiers

/// What a file added to a space is.
enum MediaKind: Equatable, Sendable {
    case photo, audio, video

    /// The kind of a file by its type: a HEIC, JPEG or PNG photo, an audio file or a video.
    /// `nil` for anything else.
    static func of(_ url: URL) -> MediaKind? {
        guard let type = UTType(filenameExtension: url.pathExtension) else { return nil }
        if [UTType.heic, .heif, .jpeg, .png].contains(where: { type.conforms(to: $0) }) { return .photo }
        if type.conforms(to: .movie) { return .video }
        if type.conforms(to: .audio) { return .audio }
        return nil
    }
}

/// The hash that finds a file again in a space, `sha256:<hex>`, the same as the core's.
enum MediaKey {
    static func of(_ url: URL) throws -> String {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var hasher = SHA256()
        while let chunk = try handle.read(upToCount: 1 << 20), !chunk.isEmpty {
            hasher.update(data: chunk)
        }
        return "sha256:" + hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }
}

/// What an audio file or a video says about itself: when it was recorded and how long it is.
protocol MediaInspector: Sendable {
    /// When it was recorded (its creation date, else the file's), and how long it is.
    func inspect(_ url: URL) async -> (startedAt: Date, duration: TimeInterval)
}

/// Takes the audio of a file that is too large to attach, as a small AAC file for transcription.
protocol AudioExtractor: Sendable {
    /// Writes the first audio track of `url` into `folder` as `<name>.m4a` and returns it.
    func extractAudio(from url: URL, named name: String, into folder: URL) async throws -> URL
}

/// Adds photos, audio files and videos to a space: each becomes one node with the original
/// attached unchanged, and the pipelines on this Mac read the photos and transcribe the rest.
///
/// A file larger than a space attaches (100 MB) is never changed. An audio file or a video that
/// large keeps only its audio, extracted on this Mac as mono AAC (about 30 MB an hour) and
/// attached in its place, so it can still be transcribed; the original stays where it was and the
/// recording says so, unless the space keeps large originals: then the core attaches the original
/// too. A photo that large, or audio still too long, is refused with why.
@MainActor
@Observable
final class MediaImporter {
    /// What one call to `add` did.
    struct Report: Equatable, Sendable {
        /// The nodes, new or found again, in order.
        var nodes: [Node.ID] = []
        /// Why each file left out was left out, for people.
        var refused: [String] = []
    }

    typealias Store = RecordingStore & PhotoStore

    /// Whether files are being added now.
    private(set) var isAdding = false

    @ObservationIgnored private let store: any Store
    @ObservationIgnored private let storage: (any SpaceStorage)?
    @ObservationIgnored private let inspector: any MediaInspector
    @ObservationIgnored private let extractor: any AudioExtractor
    @ObservationIgnored private let limit: Int64
    @ObservationIgnored private let scratch: URL
    @ObservationIgnored private let didAdd: @MainActor (_ space: URL, _ kind: MediaKind) -> Void

    init(
        store: any Store,
        storage: (any SpaceStorage)? = nil,
        inspector: any MediaInspector = AVMediaInspector(),
        extractor: any AudioExtractor = AACAudioExtractor(),
        limit: Int64 = AttachmentInfo.maxSize,
        scratch: URL = FileManager.default.temporaryDirectory.appending(path: "Knode Adding Media", directoryHint: .isDirectory),
        didAdd: @escaping @MainActor (_ space: URL, _ kind: MediaKind) -> Void = { _, _ in }
    ) {
        self.store = store
        self.storage = storage ?? (store as? any SpaceStorage)
        self.inspector = inspector
        self.extractor = extractor
        self.limit = limit
        self.scratch = scratch
        self.didAdd = didAdd
    }

    /// How deep into a folder its media are found, as `knode import` does.
    nonisolated static let folderDepth = 3
    /// The most media files taken from the folders of one `add`, as `knode import` takes.
    nonisolated static let folderLimit = 10_000

    /// Why to ask before adding these files: the space would go past its soft limit
    /// (`storage.check`). `nil` when there is room, or when the space cannot tell.
    func largeImportWarning(for urls: [URL], in space: Space.ID) async -> String? {
        guard let storage else { return nil }
        let size = urls.filter { MediaKind.of($0) != nil }.reduce(Int64(0)) { sum, url in
            sum + ((try? self.size(of: url)) ?? 0)
        }
        guard size > 0, let check = try? await storage.storageCheck(adding: size, in: space), check.warn else { return nil }
        return "Adding \(StorageText.size(size)) takes this space to \(StorageText.size(check.after)), past its limit of \(StorageText.size(check.warnAt)). You can change the limit or remove large files in Settings > Spaces > Storage."
    }

    /// Whether any of these files is one `add` takes, or a folder.
    static func accepts(_ urls: [URL]) -> Bool { urls.contains { MediaKind.of($0) != nil || isFolder($0) } }

    /// Adds each photo, audio file and video of `urls` to the space, and those of each folder
    /// (`expand`); anything else is refused.
    func add(_ urls: [URL], to space: Space.ID, at spaceURL: URL, project: Node.ID? = nil) async -> Report {
        isAdding = true
        defer { isAdding = false }
        var report = Report()
        let (files, empty) = await Task.detached(priority: .userInitiated) { Self.expand(urls) }.value
        for folder in empty {
            report.refused.append("\(folder.lastPathComponent) has no photos (HEIC, JPEG, PNG), audio files or videos.")
        }
        for url in files {
            let name = url.lastPathComponent
            guard let kind = MediaKind.of(url) else {
                report.refused.append("\(name) is not a photo (HEIC, JPEG, PNG), an audio file or a video.")
                continue
            }
            do {
                let id = switch kind {
                case .photo: try await addPhoto(url, to: space, project: project)
                case .audio, .video: try await addRecording(url, kind: kind, to: space, project: project)
                }
                report.nodes.append(id)
                didAdd(spaceURL, kind)
            } catch {
                report.refused.append("\(name) could not be added: \(error.localizedDescription)")
            }
        }
        return report
    }

    nonisolated private static func isFolder(_ url: URL) -> Bool {
        (try? url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) == true
    }

    /// `urls` with each folder replaced by its photos, audio files and videos, as `knode import`
    /// finds them: down to `folderDepth` folders deep, hidden files and folders left out, at most
    /// `folderLimit` in all, in name order. Also the folders where none were found.
    nonisolated static func expand(_ urls: [URL]) -> (files: [URL], empty: [URL]) {
        var files: [URL] = []
        var empty: [URL] = []
        var found = 0
        func walk(_ folder: URL, depth: Int) {
            guard depth <= folderDepth, found < folderLimit else { return }
            let entries = (try? FileManager.default.contentsOfDirectory(
                at: folder, includingPropertiesForKeys: [.isDirectoryKey], options: [.skipsHiddenFiles]
            )) ?? []
            for entry in entries.sorted(by: { $0.lastPathComponent < $1.lastPathComponent }) {
                if isFolder(entry) {
                    walk(entry, depth: depth + 1)
                } else if MediaKind.of(entry) != nil, found < folderLimit {
                    files.append(entry)
                    found += 1
                }
            }
        }
        for url in urls {
            guard isFolder(url) else {
                files.append(url)
                continue
            }
            let before = files.count
            walk(url, depth: 0)
            if files.count == before { empty.append(url) }
        }
        return (files, empty)
    }

    private func size(of url: URL) throws -> Int64 {
        Int64(try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0)
    }

    private func addPhoto(_ url: URL, to space: Space.ID, project: Node.ID?) async throws -> Node.ID {
        guard try size(of: url) <= limit else { throw MediaImportError.tooLarge(url.lastPathComponent, limit: limit) }
        let metadata = PhotoMetadataReader.read(url) ?? PhotoMetadataDraft()
        let draft = NewPhotoDraft(title: "", url: url, project: project, metadata: metadata)
        return try await store.createPhoto(draft, in: space).id
    }

    private func addRecording(_ url: URL, kind: MediaKind, to space: Space.ID, project: Node.ID?) async throws -> Node.ID {
        let key = try await Task.detached(priority: .utility) { try MediaKey.of(url) }.value
        let (startedAt, duration) = await inspector.inspect(url)
        let stem = url.deletingPathExtension().lastPathComponent
        var attach = url
        var original: String?
        var originalURL: URL?
        var folder: URL?
        defer { if let folder { try? FileManager.default.removeItem(at: folder) } }
        if try size(of: url) > limit {
            // Added before: found by its key, without taking its audio out again.
            if try await store.recordingID(withKey: key, in: space) != nil {
                let draft = NewRecordingDraft(
                    kind: .recording, title: stem.isEmpty ? url.lastPathComponent : stem,
                    startedAt: startedAt, endedAt: startedAt.addingTimeInterval(max(duration, 0)), project: project,
                    audio: [], key: key, app: nil, summaries: true, original: url.lastPathComponent
                )
                // The same key returns that recording, with the link to `project` added.
                return try await store.createRecording(draft, in: space).id
            }
            let temporary = scratch.appending(path: UUID().uuidString, directoryHint: .isDirectory)
            try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
            folder = temporary
            attach = try await extractor.extractAudio(from: url, named: stem, into: temporary)
            guard try size(of: attach) <= limit else { throw MediaImportError.audioTooLong(url.lastPathComponent, limit: limit) }
            original = url.lastPathComponent
            originalURL = url
        }
        let draft = NewRecordingDraft(
            kind: .recording, title: stem.isEmpty ? url.lastPathComponent : stem,
            startedAt: startedAt, endedAt: startedAt.addingTimeInterval(max(duration, 0)), project: project,
            audio: [RecordedTrack(role: .mixed, url: attach)], key: key, app: nil,
            summaries: (try? await store.recordingSummariesEnabled(in: space)) ?? true,
            original: original, originalURL: originalURL
        )
        return try await store.createRecording(draft, in: space).id
    }
}

enum MediaImportError: Error, LocalizedError, Equatable {
    case tooLarge(String, limit: Int64)
    case audioTooLong(String, limit: Int64)

    var errorDescription: String? {
        switch self {
        case .tooLarge(let name, let limit):
            "\(name) is larger than \(limit / (1024 * 1024)) MB, the most a space keeps."
        case .audioTooLong(let name, let limit):
            "even its audio alone is larger than \(limit / (1024 * 1024)) MB, the most a space keeps; \(name) was not changed."
        }
    }
}

/// Reads when a file was recorded and how long it is with AVFoundation.
struct AVMediaInspector: MediaInspector {
    func inspect(_ url: URL) async -> (startedAt: Date, duration: TimeInterval) {
        let asset = AVURLAsset(url: url)
        let duration = (try? await asset.load(.duration)).map { $0.isNumeric ? $0.seconds : 0 } ?? 0
        var created: Date?
        if let item = try? await asset.load(.creationDate) {
            created = try? await item.load(.dateValue)
        }
        let values = try? url.resourceValues(forKeys: [.creationDateKey, .contentModificationDateKey])
        let started = created ?? values?.creationDate ?? values?.contentModificationDate ?? .now
        return (started, duration)
    }
}

/// Extracts the first audio track as mono AAC at 64 kbit/s with `AVAssetReader` and
/// `AVAssetWriter`. The original file is only read.
struct AACAudioExtractor: AudioExtractor {
    static let bitRate = 64_000

    func extractAudio(from url: URL, named name: String, into folder: URL) async throws -> URL {
        let output = folder.appending(path: "\(name.isEmpty ? "audio" : name).m4a")
        try? FileManager.default.removeItem(at: output)
        let asset = AVURLAsset(url: url)
        guard let track = try await asset.loadTracks(withMediaType: .audio).first else {
            throw TranscriptionError.unreadableAudio("the file has no audio")
        }
        let reader = try AVAssetReader(asset: asset)
        let readerOutput = AVAssetReaderTrackOutput(track: track, outputSettings: [
            AVFormatIDKey: kAudioFormatLinearPCM,
            AVSampleRateKey: 44_100,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsFloatKey: false,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsNonInterleaved: false,
        ])
        guard reader.canAdd(readerOutput) else { throw TranscriptionError.unreadableAudio("the audio cannot be decoded") }
        reader.add(readerOutput)
        let writer = try AVAssetWriter(outputURL: output, fileType: .m4a)
        let input = AVAssetWriterInput(mediaType: .audio, outputSettings: [
            AVFormatIDKey: kAudioFormatMPEG4AAC,
            AVSampleRateKey: 44_100,
            AVNumberOfChannelsKey: 1,
            AVEncoderBitRateKey: Self.bitRate,
        ])
        input.expectsMediaDataInRealTime = false
        guard writer.canAdd(input) else { throw TranscriptionError.unreadableAudio("the audio cannot be encoded") }
        writer.add(input)
        guard reader.startReading() else {
            throw TranscriptionError.unreadableAudio(reader.error?.localizedDescription ?? "the audio cannot be decoded")
        }
        guard writer.startWriting() else {
            throw TranscriptionError.unreadableAudio(writer.error?.localizedDescription ?? "the audio cannot be encoded")
        }
        writer.startSession(atSourceTime: .zero)
        let pump = SamplePump(reader: reader, output: readerOutput, input: input)
        await pump.run()
        if reader.status == .failed {
            writer.cancelWriting()
            throw TranscriptionError.unreadableAudio(reader.error?.localizedDescription ?? "the audio cannot be decoded")
        }
        await writer.finishWriting()
        if writer.status != .completed {
            throw TranscriptionError.unreadableAudio(writer.error?.localizedDescription ?? "the audio cannot be encoded")
        }
        return output
    }
}

/// Moves samples from a reader to a writer on a queue of its own, as fast as the writer takes them.
private final class SamplePump: @unchecked Sendable {
    private let reader: AVAssetReader
    private let output: AVAssetReaderTrackOutput
    private let input: AVAssetWriterInput
    private let queue = DispatchQueue(label: "md.knode.audio-extract")
    /// Read and written on `queue` only.
    private var finished = false

    init(reader: AVAssetReader, output: AVAssetReaderTrackOutput, input: AVAssetWriterInput) {
        self.reader = reader
        self.output = output
        self.input = input
    }

    func run() async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            input.requestMediaDataWhenReady(on: queue) { [self] in
                guard !finished else { return }
                while input.isReadyForMoreMediaData {
                    if let sample = output.copyNextSampleBuffer(), input.append(sample) { continue }
                    finished = true
                    input.markAsFinished()
                    if reader.status == .reading { reader.cancelReading() }
                    continuation.resume()
                    return
                }
            }
        }
    }
}
