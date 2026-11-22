import AVFoundation
import CoreGraphics
import CoreText
import Foundation
import ImageIO
import Synchronization
import Testing
import UniformTypeIdentifiers
@testable import Knode

// MARK: Fakes

/// A space's photos in memory, as the core keeps them.
actor FakePhotosBackend: PhotosBackend {
    let space: Space
    let folder: URL
    var photos: [Node.ID: StoredPhoto] = [:]
    var readings: [Node.ID: PhotoReadingDraft] = [:]
    var failures: [Node.ID: String] = [:]
    private var counter = 0
    private nonisolated let broadcast = Broadcast<SpaceChange>()

    init() throws {
        folder = FileManager.default.temporaryDirectory.appending(path: "knode-photos-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        space = Space(id: "space-1", name: "Work", url: folder.appending(path: "Work.knode", directoryHint: .isDirectory))
    }

    nonisolated var spaceURL: URL { space.url! }

    /// A pending photo, as `knode import` leaves one; `file` false leaves its file out.
    @discardableResult
    func add(name: String, file: Bool = true, announce: Bool = false) throws -> Node.ID {
        counter += 1
        let id = "photo-\(counter)"
        if file { try Data("image".utf8).write(to: folder.appending(path: "\(id)-\(name)")) }
        photos[id] = StoredPhoto(id: id, title: "Photo \(counter)", file: name, metadata: PhotoMetadataDraft(), reading: .pending, error: nil, project: nil)
        if announce { broadcast.yield(SpaceChange(space: space.id, nodeID: id, kind: .create)) }
        return id
    }

    func openSpace(at url: URL) async throws -> Space { space }

    func attachmentURL(named name: String, of node: Node.ID, in space: Space.ID) async throws -> URL? {
        let url = folder.appending(path: "\(node)-\(name)")
        return FileManager.default.fileExists(atPath: url.path(percentEncoded: false)) ? url : nil
    }

    nonisolated func changes() -> AsyncStream<SpaceChange> { broadcast.stream() }

    func createPhoto(_ photo: NewPhotoDraft, in space: Space.ID) async throws -> StoredPhoto {
        counter += 1
        let stored = StoredPhoto(id: "photo-\(counter)", title: photo.title, file: photo.url.lastPathComponent, metadata: photo.metadata, reading: .pending, error: nil, project: photo.project)
        photos[stored.id] = stored
        return stored
    }

    func pendingPhotos(in space: Space.ID) async throws -> [StoredPhoto] {
        photos.values.filter { $0.reading == .pending }.sorted { $0.id < $1.id }
    }

    func failedPhotos(in space: Space.ID) async throws -> [StoredPhoto] {
        photos.values.filter { $0.reading == .failed }.sorted { $0.id > $1.id }
    }

    func photo(_ id: Node.ID, in space: Space.ID) async throws -> StoredPhoto {
        guard let photo = photos[id] else { throw SpaceStoreError.nodeNotFound(id) }
        return photo
    }

    func setPhotoReading(_ reading: PhotoReadingDraft, of id: Node.ID, in space: Space.ID) async throws -> StoredPhoto {
        readings[id] = reading
        photos[id]?.reading = .done
        photos[id]?.error = nil
        return try await photo(id, in: space)
    }

    func failPhoto(_ id: Node.ID, error: String, in space: Space.ID) async throws -> StoredPhoto {
        failures[id] = error
        photos[id]?.reading = .failed
        photos[id]?.error = error
        return try await photo(id, in: space)
    }

    func retryPhoto(_ id: Node.ID, in space: Space.ID) async throws -> StoredPhoto {
        photos[id]?.reading = .pending
        photos[id]?.error = nil
        return try await photo(id, in: space)
    }
}

/// Reads every photo as a whiteboard, or fails for the names it is told to.
final class FakePhotoReader: PhotoReader {
    let state = Mutex<(read: [String], failing: Set<String>)>(([], []))

    func read(_ url: URL) async throws -> PhotoReadingDraft {
        let name = url.lastPathComponent
        let fails = state.withLock { state -> Bool in
            state.read.append(name)
            return state.failing.contains(name)
        }
        if fails { throw PhotoReadingError.unreadableImage(name) }
        return PhotoReadingDraft(text: "Roadmap", labels: ["whiteboard"], description: nil, metadata: PhotoMetadataDraft(width: 4))
    }
}

/// Recordings and photos as a space stores them, for the importer.
actor FakeMediaStore: RecordingStore, PhotoStore {
    var recordingDrafts: [NewRecordingDraft] = []
    var photoDrafts: [NewPhotoDraft] = []
    /// The size of each audio file as it was handed over, which may be a temporary file.
    var attachedSizes: [Int] = []
    var summariesOn = true

    func createRecording(_ recording: NewRecordingDraft, in space: Space.ID) async throws -> StoredRecording {
        recordingDrafts.append(recording)
        for track in recording.audio {
            attachedSizes.append((try? Data(contentsOf: track.url).count) ?? -1)
        }
        return StoredRecording(
            id: "rec-\(recordingDrafts.count)", kind: recording.kind, title: recording.title, startedAt: recording.startedAt,
            endedAt: recording.endedAt, language: nil, transcript: .pending, summary: .pending, error: nil,
            project: recording.project, tracks: [], transcriptID: nil
        )
    }

    func pendingRecordings(in space: Space.ID) async throws -> [StoredRecording] { [] }
    func failedRecordings(in space: Space.ID) async throws -> [StoredRecording] { [] }
    func recording(_ id: Node.ID, in space: Space.ID) async throws -> StoredRecording { throw SpaceStoreError.nodeNotFound(id) }
    func setTranscript(_ transcript: Transcript, by engine: RecordingEngine, of id: Node.ID, in space: Space.ID) async throws -> StoredRecording { throw SpaceStoreError.nodeNotFound(id) }
    func setSummary(_ summary: MeetingSummary, by engine: RecordingEngine, of id: Node.ID, in space: Space.ID) async throws -> StoredRecording { throw SpaceStoreError.nodeNotFound(id) }
    func failRecording(_ id: Node.ID, stage: StoredRecording.Stage, failure: StoredRecording.Failure, error: String, by engine: RecordingEngine, in space: Space.ID) async throws -> StoredRecording { throw SpaceStoreError.nodeNotFound(id) }
    func retryRecording(_ id: Node.ID, stage: StoredRecording.Stage, in space: Space.ID) async throws -> StoredRecording { throw SpaceStoreError.nodeNotFound(id) }
    func recordingSummariesEnabled(in space: Space.ID) async throws -> Bool { summariesOn }
    func setRecordingSummariesEnabled(_ enabled: Bool, in space: Space.ID) async throws { summariesOn = enabled }

    func createPhoto(_ photo: NewPhotoDraft, in space: Space.ID) async throws -> StoredPhoto {
        photoDrafts.append(photo)
        return StoredPhoto(id: "photo-\(photoDrafts.count)", title: photo.title, file: photo.url.lastPathComponent, metadata: photo.metadata, reading: .pending, error: nil, project: photo.project)
    }

    func pendingPhotos(in space: Space.ID) async throws -> [StoredPhoto] { [] }
    func failedPhotos(in space: Space.ID) async throws -> [StoredPhoto] { [] }
    func photo(_ id: Node.ID, in space: Space.ID) async throws -> StoredPhoto { throw SpaceStoreError.nodeNotFound(id) }
    func setPhotoReading(_ reading: PhotoReadingDraft, of id: Node.ID, in space: Space.ID) async throws -> StoredPhoto { throw SpaceStoreError.nodeNotFound(id) }
    func failPhoto(_ id: Node.ID, error: String, in space: Space.ID) async throws -> StoredPhoto { throw SpaceStoreError.nodeNotFound(id) }
    func retryPhoto(_ id: Node.ID, in space: Space.ID) async throws -> StoredPhoto { throw SpaceStoreError.nodeNotFound(id) }
}

struct FakeInspector: MediaInspector {
    static let start = Date(timeIntervalSince1970: 1_795_343_400)
    func inspect(_ url: URL) async -> (startedAt: Date, duration: TimeInterval) { (Self.start, 90) }
}

/// Writes `size` bytes as the extracted audio, and remembers what it was asked for.
final class FakeExtractor: AudioExtractor {
    let size: Int
    let calls = Mutex<[String]>([])
    init(size: Int) { self.size = size }

    func extractAudio(from url: URL, named name: String, into folder: URL) async throws -> URL {
        calls.withLock { $0.append(url.lastPathComponent) }
        let output = folder.appending(path: "\(name).m4a")
        try Data(repeating: 1, count: size).write(to: output)
        return output
    }
}

func mediaFolder() throws -> URL {
    let folder = FileManager.default.temporaryDirectory.appending(path: "KnodeMedia-\(UUID().uuidString)", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    return folder
}

/// A white PNG with `text` drawn on it in large black letters.
func renderTextImage(_ text: String, to url: URL) throws {
    let width = 1400, height = 400
    let context = try #require(CGContext(
        data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ))
    context.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
    context.fill(CGRect(x: 0, y: 0, width: width, height: height))
    let font = CTFontCreateWithName("Helvetica-Bold" as CFString, 96, nil)
    let attributes: [NSAttributedString.Key: Any] = [
        NSAttributedString.Key(kCTFontAttributeName as String): font,
        NSAttributedString.Key(kCTForegroundColorAttributeName as String): CGColor(red: 0, green: 0, blue: 0, alpha: 1),
    ]
    let line = CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: attributes))
    context.textPosition = CGPoint(x: 60, y: 160)
    CTLineDraw(line, context)
    let image = try #require(context.makeImage())
    let destination = try #require(CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil))
    CGImageDestinationAddImage(destination, image, nil)
    #expect(CGImageDestinationFinalize(destination))
}

// MARK: Photos processor

@MainActor
struct PhotosProcessorTests {
    @Test func readsAPendingPhotoAndWritesWhatItFound() async throws {
        let store = try FakePhotosBackend()
        let id = try await store.add(name: "board.heic")
        let reader = FakePhotoReader()
        let url = store.spaceURL
        let processor = PhotosProcessor(store: store, reader: reader, spaceURLs: { [url] })
        processor.start()
        #expect(try await eventually { await store.photos[id]?.reading == .done })
        #expect(await store.readings[id]?.text == "Roadmap")
        #expect(await store.readings[id]?.labels == ["whiteboard"])
        // Read under its own name, not the space's hash.
        #expect(reader.state.withLock { $0.read } == ["board.heic"])
        #expect(processor.failures.isEmpty)
        await processor.stop()
    }

    @Test func aPhotoThatCannotBeReadIsMarkedFailedAndCanBeRetried() async throws {
        let store = try FakePhotosBackend()
        let id = try await store.add(name: "broken.jpg")
        let reader = FakePhotoReader()
        reader.state.withLock { $0.failing = ["broken.jpg"] }
        let url = store.spaceURL
        let processor = PhotosProcessor(store: store, reader: reader, spaceURLs: { [url] })
        processor.start()
        #expect(try await eventually { await store.photos[id]?.reading == .failed })
        #expect(await store.failures[id] == "broken.jpg could not be read as an image.")
        #expect(try await eventually { processor.failures.map(\.photo) == [id] })

        reader.state.withLock { $0.failing = [] }
        try await processor.retry(try #require(processor.failures.first))
        #expect(try await eventually { await store.photos[id]?.reading == .done })
        #expect(processor.failures.isEmpty)
        await processor.stop()
    }

    @Test func aMissingFileFailsTheReading() async throws {
        let store = try FakePhotosBackend()
        let id = try await store.add(name: "gone.png", file: false)
        let url = store.spaceURL
        let processor = PhotosProcessor(store: store, reader: FakePhotoReader(), spaceURLs: { [url] })
        processor.start()
        #expect(try await eventually { await store.photos[id]?.reading == .failed })
        #expect(await store.failures[id] == "The photo gone.png is missing from the space.")
        await processor.stop()
    }

    @Test func readsAPhotoAnnouncedLater() async throws {
        let store = try FakePhotosBackend()
        let url = store.spaceURL
        let processor = PhotosProcessor(store: store, reader: FakePhotoReader(), spaceURLs: { [url] })
        processor.start()
        await processor.idle()
        let id = try await store.add(name: "later.png", announce: true)
        #expect(try await eventually { await store.photos[id]?.reading == .done })
        await processor.stop()
    }
}

// MARK: Importer

@MainActor
struct MediaImporterTests {
    @Test func kindsByType() {
        #expect(MediaKind.of(URL(filePath: "/a/IMG_1.HEIC")) == .photo)
        #expect(MediaKind.of(URL(filePath: "/a/b.jpeg")) == .photo)
        #expect(MediaKind.of(URL(filePath: "/a/b.png")) == .photo)
        #expect(MediaKind.of(URL(filePath: "/a/talk.mov")) == .video)
        #expect(MediaKind.of(URL(filePath: "/a/talk.mp4")) == .video)
        #expect(MediaKind.of(URL(filePath: "/a/memo.m4a")) == .audio)
        #expect(MediaKind.of(URL(filePath: "/a/memo.mp3")) == .audio)
        #expect(MediaKind.of(URL(filePath: "/a/notes.md")) == nil)
        #expect(AudioReading.isVideo(URL(filePath: "/a/0-talk.mov")))
        #expect(!AudioReading.isVideo(URL(filePath: "/a/0-memo.m4a")))
    }

    @Test func keysAreTheSha256OfTheFileLikeTheCore() throws {
        let folder = try mediaFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appending(path: "a.png")
        try Data("abc".utf8).write(to: file)
        #expect(try MediaKey.of(file) == "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    }

    @Test func anAudioFileBecomesARecordingWithItsLengthAndKey() async throws {
        let folder = try mediaFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let memo = folder.appending(path: "Walk idea.m4a")
        try Data("abc".utf8).write(to: memo)
        let store = FakeMediaStore()
        await store.setSummaries(false)
        let added = Mutex<[MediaKind]>([])
        let importer = MediaImporter(store: store, inspector: FakeInspector(), extractor: FakeExtractor(size: 1), limit: 100, scratch: folder.appending(path: "scratch")) { _, kind in added.withLock { $0.append(kind) } }
        let report = await importer.add([memo], to: "space-1", at: folder, project: "project-1")
        #expect(report == MediaImporter.Report(nodes: ["rec-1"], refused: []))
        #expect(added.withLock { $0 } == [.audio])
        let draft = try #require(await store.recordingDrafts.first)
        #expect(draft.kind == .recording)
        #expect(draft.title == "Walk idea")
        #expect(draft.startedAt == FakeInspector.start)
        #expect(draft.endedAt == FakeInspector.start.addingTimeInterval(90))
        #expect(draft.audio == [RecordedTrack(role: .mixed, url: memo)])
        #expect(draft.key == "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        #expect(draft.project == "project-1")
        #expect(draft.summaries == false)
        #expect(draft.original == nil)
    }

    @Test func aVideoTooLargeToAttachKeepsItsAudioAndTheOriginalUnchanged() async throws {
        let folder = try mediaFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let video = folder.appending(path: "Talk.mov")
        let bytes = Data(repeating: 7, count: 500)
        try bytes.write(to: video)
        let store = FakeMediaStore()
        let extractor = FakeExtractor(size: 40)
        let scratch = folder.appending(path: "scratch")
        let importer = MediaImporter(store: store, inspector: FakeInspector(), extractor: extractor, limit: 100, scratch: scratch)
        let report = await importer.add([video], to: "space-1", at: folder)
        #expect(report.refused.isEmpty)
        #expect(extractor.calls.withLock { $0 } == ["Talk.mov"])
        let draft = try #require(await store.recordingDrafts.first)
        #expect(draft.audio.map(\.url.lastPathComponent) == ["Talk.m4a"])
        #expect(draft.original == "Talk.mov")
        #expect(draft.key == (try MediaKey.of(video)))
        #expect(await store.attachedSizes == [40])
        // The original is never changed, and the extracted audio is cleaned up.
        #expect(try Data(contentsOf: video) == bytes)
        #expect((try? FileManager.default.contentsOfDirectory(atPath: scratch.path(percentEncoded: false)))?.isEmpty ?? true)
    }

    @Test func refusesWhatItCannotKeep() async throws {
        let folder = try mediaFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let long = folder.appending(path: "Long.mp4")
        try Data(repeating: 1, count: 500).write(to: long)
        let photo = folder.appending(path: "Huge.jpg")
        try Data(repeating: 1, count: 500).write(to: photo)
        let text = folder.appending(path: "notes.txt")
        try Data("x".utf8).write(to: text)
        let store = FakeMediaStore()
        let importer = MediaImporter(store: store, inspector: FakeInspector(), extractor: FakeExtractor(size: 200), limit: 100, scratch: folder.appending(path: "scratch"))
        let report = await importer.add([long, photo, text], to: "space-1", at: folder)
        #expect(report.nodes.isEmpty)
        #expect(report.refused.count == 3)
        #expect(report.refused[0].contains("even its audio alone is larger than"))
        #expect(report.refused[1].contains("Huge.jpg is larger than"))
        #expect(report.refused[2].contains("notes.txt is not a photo"))
        #expect(await store.recordingDrafts.isEmpty)
        #expect(await store.photoDrafts.isEmpty)
    }

    @Test func aPhotoIsAddedWithWhatItsFileSays() async throws {
        let folder = try mediaFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let photo = folder.appending(path: "Board.png")
        try renderTextImage("Hi", to: photo)
        let store = FakeMediaStore()
        let importer = MediaImporter(store: store, inspector: FakeInspector(), extractor: FakeExtractor(size: 1), scratch: folder.appending(path: "scratch"))
        let report = await importer.add([photo], to: "space-1", at: folder)
        #expect(report.nodes == ["photo-1"])
        let draft = try #require(await store.photoDrafts.first)
        #expect(draft.url == photo)
        #expect(draft.metadata.width == 1400)
        #expect(draft.metadata.height == 400)
    }
}

extension FakeMediaStore {
    func setSummaries(_ on: Bool) { summariesOn = on }
}

// MARK: Reading on the device

struct PhotoReadingTests {
    @Test func exifDateGpsAndCamera() throws {
        let properties: [CFString: Any] = [
            kCGImagePropertyPixelWidth: 4032, kCGImagePropertyPixelHeight: 3024,
            kCGImagePropertyExifDictionary: [
                kCGImagePropertyExifDateTimeOriginal: "2026:11:22 11:30:00",
                kCGImagePropertyExifOffsetTimeOriginal: "+01:00",
            ] as [CFString: Any],
            kCGImagePropertyTIFFDictionary: [kCGImagePropertyTIFFMake: "Apple", kCGImagePropertyTIFFModel: "iPhone 16 Pro"] as [CFString: Any],
            kCGImagePropertyGPSDictionary: [
                kCGImagePropertyGPSLatitude: 40.4168, kCGImagePropertyGPSLatitudeRef: "N",
                kCGImagePropertyGPSLongitude: 3.7038, kCGImagePropertyGPSLongitudeRef: "W",
            ] as [CFString: Any],
        ]
        let metadata = PhotoMetadataReader.metadata(properties)
        #expect(metadata.takenAt == Date(timeIntervalSince1970: 1_795_343_400))
        #expect(metadata.latitude == 40.4168)
        #expect(metadata.longitude == -3.7038)
        #expect(metadata.camera == "Apple iPhone 16 Pro")
        #expect(metadata.width == 4032)
        #expect(metadata.place == nil)
    }

    /// Vision on this Mac, no network: the text drawn into an image is read back.
    @Test func visionReadsTheTextInAPhoto() async throws {
        let folder = try mediaFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let url = folder.appending(path: "whiteboard.png")
        try renderTextImage("KNODE ROADMAP 2026", to: url)
        let reading = try await VisionPhotoReader().read(url)
        #expect(reading.text.uppercased().contains("KNODE"))
        #expect(reading.text.contains("2026"))
        #expect(reading.metadata.width == 1400)
        #expect(reading.labels.count <= 8)
    }

    @Test func anUnreadableImageSaysSo() async throws {
        let folder = try mediaFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let url = folder.appending(path: "broken.jpg")
        try Data("not an image".utf8).write(to: url)
        await #expect(throws: PhotoReadingError.unreadableImage("broken.jpg")) { try await VisionPhotoReader().read(url) }
    }

    /// AVFoundation on this Mac: the audio of a file is written as a small AAC file, and the
    /// original is left as it was.
    @Test func extractsAudioAsAAC() async throws {
        let folder = try mediaFolder()
        defer { try? FileManager.default.removeItem(at: folder) }
        let wav = folder.appending(path: "tone.wav")
        let format = AVAudioFormat(standardFormatWithSampleRate: 44_100, channels: 1)!
        do {
            let file = try AVAudioFile(forWriting: wav, settings: format.settings)
            let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 44_100 * 2)!
            buffer.frameLength = buffer.frameCapacity
            let samples = buffer.floatChannelData![0]
            for i in 0..<Int(buffer.frameLength) { samples[i] = sin(Float(i) * 2 * .pi * 440 / 44_100) * 0.5 }
            try file.write(from: buffer)
        }
        let before = try Data(contentsOf: wav)
        let output = try await AACAudioExtractor().extractAudio(from: wav, named: "tone", into: folder.appending(path: "out", directoryHint: .isDirectory).creatingDirectory())
        #expect(output.lastPathComponent == "tone.m4a")
        let size = try #require(try output.resourceValues(forKeys: [.fileSizeKey]).fileSize)
        #expect(size > 0 && size < before.count)
        let duration = try await AVURLAsset(url: output).load(.duration).seconds
        #expect(abs(duration - 2) < 0.2)
        #expect(try Data(contentsOf: wav) == before)
    }
}

private extension URL {
    func creatingDirectory() throws -> URL {
        try FileManager.default.createDirectory(at: self, withIntermediateDirectories: true)
        return self
    }
}

// MARK: Photos in the core

final class CorePhotosTests {
    let folder: URL

    init() throws { folder = try mediaFolder() }
    deinit { try? FileManager.default.removeItem(at: folder) }

    @Test func addsReadsAndRetriesAPhoto() async throws {
        let store = CoreSpaceStore(person: "Ada")
        let space = try await store.createSpace(at: folder.appending(path: "Test.knode", directoryHint: .isDirectory), name: "Test")
        let file = folder.appending(path: "Board.png")
        try renderTextImage("Hi", to: file)
        let draft = NewPhotoDraft(title: "", url: file, project: nil, metadata: PhotoMetadataDraft(takenAt: Date(timeIntervalSince1970: 1_795_343_400), width: 1400, height: 400))
        let created = try await store.createPhoto(draft, in: space.id)
        #expect(created.title == "Board")
        #expect(created.file == "Board.png")
        #expect(created.reading == .pending)
        #expect(try await store.createPhoto(draft, in: space.id).id == created.id)
        #expect(try await store.pendingPhotos(in: space.id).map(\.id) == [created.id])
        let blob = try #require(try await store.attachmentURL(named: "Board.png", of: created.id, in: space.id))
        #expect(try Data(contentsOf: blob) == Data(contentsOf: file))

        let reading = PhotoReadingDraft(text: "Roadmap", labels: ["whiteboard"], description: nil, metadata: PhotoMetadataDraft(latitude: 40.4, longitude: -3.7))
        let read = try await store.setPhotoReading(reading, of: created.id, in: space.id)
        #expect(read.reading == .done)
        #expect(read.metadata.latitude == 40.4)
        let node = try await store.node(created.id, in: space.id)
        #expect(node.body.contains("## Text in the photo\n\nRoadmap"))
        #expect(node.body.contains("_Read on this Mac by Apple Vision._"))
        let history = try await store.history(of: created.id, in: space.id)
        #expect(history.contains { $0.author == Author(kind: .system, name: "photos", source: "apple-vision") })

        #expect(try await store.failPhoto(created.id, error: "Unreadable", in: space.id).reading == .failed)
        #expect(try await store.failedPhotos(in: space.id).map(\.id) == [created.id])
        #expect(try await store.retryPhoto(created.id, in: space.id).reading == .pending)
    }
}
