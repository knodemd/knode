import AVFAudio
import CoreAudio
import Foundation
import Synchronization

/// Why a recording could not start or finish.
enum RecordingError: Error, LocalizedError, Equatable {
    case microphonePermissionDenied
    case audioCapturePermissionDenied
    case noInputDevice
    case systemAudioUnavailable(String)
    case alreadyRecording
    case notRecording
    case writeFailed(String)
    case tooLarge(file: String, size: Int64, limit: Int64, folder: String)
    case notStored(files: [String], folder: String)

    var errorDescription: String? {
        switch self {
        case .microphonePermissionDenied:
            "Knode cannot use the microphone. Allow it in System Settings > Privacy & Security > Microphone."
        case .audioCapturePermissionDenied:
            "Knode cannot record the system audio. Allow it in System Settings > Privacy & Security > Screen & System Audio Recording."
        case .noInputDevice:
            "There is no microphone to record from."
        case .systemAudioUnavailable(let reason):
            "The system audio cannot be recorded: \(reason)."
        case .alreadyRecording:
            "A recording is already running."
        case .notRecording:
            "Nothing is being recorded."
        case .writeFailed(let reason):
            "The recording could not be written: \(reason)."
        case .tooLarge(let file, let size, let limit, let folder):
            "The recording is too long to keep: \(file) is \(size.formatted(.byteCount(style: .file))), and a space keeps files up to \(limit.formatted(.byteCount(style: .file))) (about three hours of a meeting). Nothing was saved; the audio is still in \(folder)."
        case .notStored(let files, let folder):
            "The space did not keep every audio file of the recording (\(files.joined(separator: ", ")) missing). The audio is still in \(folder), and Knode can save it again."
        }
    }
}

/// Records a meeting's audio into files. `MeetingRecorder` is the real one; tests use fakes.
protocol MeetingRecording: AnyObject, Sendable {
    /// Starts recording, asking for the microphone permission the first time.
    func start(_ mode: RecordingMode) async throws
    func pause()
    func resume()
    /// Stops and returns the finished files, closed so they can be read.
    func stop() async throws -> [RecordedTrack]
    /// Stops and throws the files away.
    func cancel()
    /// How long it has recorded, pauses left out.
    var elapsed: Duration { get }
    /// The loudest track's level now, from 0 to 1, for a meter.
    var level: Float { get }
    var isPaused: Bool { get }
    /// How many times the microphone changed while recording (a headset connected, say); the
    /// recording goes on with the new one.
    var inputChanges: Int { get }
}

/// Records the microphone with `AVAudioEngine` and, for an online meeting, the system audio through
/// a Core Audio process tap, each into its own AAC file (`microphone.m4a`, `system.m4a`) in a
/// private temporary folder. In person, only the microphone is recorded (`mixed`).
///
/// The system audio permission has no status to read: macOS asks the first time the tap starts,
/// and when it is refused the tap delivers silence. When the microphone changes, the engine is
/// started again with the new input and the same file goes on.
final class MeetingRecorder: MeetingRecording, @unchecked Sendable {
    private struct Session {
        var folder: URL
        var microphone: MicrophoneCapture
        var system: TrackWriter?
        var tap: SystemAudioTap?
        var mode: RecordingMode
    }

    private struct Clock {
        var accumulated: Duration = .zero
        var runningSince: ContinuousClock.Instant?
    }

    private let state = Mutex<Session?>(nil)
    private let clock = Mutex(Clock())
    private let paused = PauseFlag()
    private let root: URL

    init(root: URL = FileManager.default.temporaryDirectory.appending(path: "Knode Recordings", directoryHint: .isDirectory)) {
        self.root = root
    }

    var isPaused: Bool { paused.isOn }

    var elapsed: Duration {
        clock.withLock { clock in
            clock.accumulated + (clock.runningSince.map { ContinuousClock.now - $0 } ?? .zero)
        }
    }

    var level: Float {
        state.withLock { session in
            guard let session else { return 0 }
            return max(session.microphone.writer.level, session.system?.level ?? 0)
        }
    }

    var inputChanges: Int {
        state.withLock { $0?.microphone.changes ?? 0 }
    }

    func start(_ mode: RecordingMode) async throws {
        guard state.withLock({ $0 == nil }) else { throw RecordingError.alreadyRecording }
        try await Self.requestMicrophone()

        let folder = root.appending(path: UUID().uuidString, directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        do {
            let session = try makeSession(mode, in: folder)
            state.withLock { $0 = session }
        } catch {
            try? FileManager.default.removeItem(at: folder)
            throw error
        }
        paused.set(false)
        clock.withLock { $0 = Clock(accumulated: .zero, runningSince: .now) }
    }

    private func makeSession(_ mode: RecordingMode, in folder: URL) throws -> Session {
        let role: RecordedTrack.Role = mode == .inPerson ? .mixed : .microphone
        let microphone = MicrophoneCapture(writer: try TrackWriter(role: role, url: folder.appending(path: "microphone.m4a")), paused: paused)
        var system: TrackWriter?
        var tap: SystemAudioTap?
        if case .online(let source) = mode {
            let systemTap = SystemAudioTap()
            _ = try systemTap.prepare(source: source)
            let writer: TrackWriter
            do {
                writer = try TrackWriter(role: .system, url: folder.appending(path: "system.m4a"))
            } catch {
                systemTap.stop()
                throw error
            }
            let paused = self.paused
            try systemTap.start { buffer in
                guard !paused.isOn else { return }
                writer.append(buffer)
            }
            system = writer
            tap = systemTap
        }
        do {
            try microphone.start()
        } catch {
            tap?.stop()
            throw error
        }
        return Session(folder: folder, microphone: microphone, system: system, tap: tap, mode: mode)
    }

    func pause() {
        guard !paused.exchange(true) else { return }
        clock.withLock { clock in
            if let since = clock.runningSince { clock.accumulated += ContinuousClock.now - since }
            clock.runningSince = nil
        }
    }

    func resume() {
        guard paused.exchange(false) else { return }
        clock.withLock { $0.runningSince = .now }
    }

    func stop() async throws -> [RecordedTrack] {
        guard let session = state.withLock({ session in defer { session = nil }; return session }) else {
            throw RecordingError.notRecording
        }
        pause()
        session.microphone.stop()
        session.tap?.stop()
        var tracks: [RecordedTrack] = []
        for writer in [session.microphone.writer, session.system].compactMap({ $0 }) {
            try writer.finish()
            tracks.append(RecordedTrack(role: writer.role, url: writer.url))
        }
        return tracks
    }

    func cancel() {
        guard let session = state.withLock({ session in defer { session = nil }; return session }) else { return }
        pause()
        session.microphone.stop()
        session.tap?.stop()
        try? session.microphone.writer.finish()
        try? session.system?.finish()
        try? FileManager.default.removeItem(at: session.folder)
    }

    private static func requestMicrophone() async throws {
        switch AVAudioApplication.shared.recordPermission {
        case .granted:
            return
        case .denied:
            throw RecordingError.microphonePermissionDenied
        default:
            guard await AVAudioApplication.requestRecordPermission() else { throw RecordingError.microphonePermissionDenied }
        }
    }
}

/// The microphone through `AVAudioEngine`, into one track. When the input changes (AirPods
/// connect, a USB microphone is unplugged) the engine stops and posts
/// `AVAudioEngineConfigurationChange`; the tap is installed again with the new input's format and
/// the engine started again, and the writer resamples whatever comes into its file's format.
final class MicrophoneCapture: @unchecked Sendable {
    let writer: TrackWriter
    private let paused: PauseFlag
    private let lock = NSLock()
    private var engine: AVAudioEngine?
    private var observer: (any NSObjectProtocol)?
    private var changeCount = 0

    init(writer: TrackWriter, paused: PauseFlag) {
        self.writer = writer
        self.paused = paused
    }

    var changes: Int { lock.withLock { changeCount } }

    func start() throws {
        let engine = AVAudioEngine()
        try run(engine)
        let observer = NotificationCenter.default.addObserver(forName: .AVAudioEngineConfigurationChange, object: engine, queue: nil) { [weak self] _ in
            self?.restart()
        }
        lock.withLock {
            self.engine = engine
            self.observer = observer
        }
    }

    func stop() {
        let (engine, observer) = lock.withLock { () -> (AVAudioEngine?, (any NSObjectProtocol)?) in
            defer { self.engine = nil; self.observer = nil }
            return (self.engine, self.observer)
        }
        if let observer { NotificationCenter.default.removeObserver(observer) }
        engine?.inputNode.removeTap(onBus: 0)
        engine?.stop()
    }

    /// The input changed: the same engine, its tap installed again with the new format.
    private func restart() {
        guard let engine = lock.withLock({ self.engine }) else { return }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        // Without an input now (the only microphone was unplugged) the track waits silently;
        // the next change starts it again.
        guard (try? run(engine)) != nil else { return }
        lock.withLock { changeCount += 1 }
    }

    private func run(_ engine: AVAudioEngine) throws {
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.channelCount > 0, format.sampleRate > 0 else { throw RecordingError.noInputDevice }
        let writer = writer
        let paused = paused
        input.installTap(onBus: 0, bufferSize: 4096, format: format) { buffer, _ in
            guard !paused.isOn else { return }
            writer.append(buffer)
        }
        do {
            engine.prepare()
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            throw RecordingError.noInputDevice
        }
    }
}

/// Writes one track: mixes each buffer down to mono, resamples it to 48 kHz and encodes it to AAC
/// on its own queue, off the audio thread, and keeps the level of the latest buffer for the meter.
/// The file's format never changes: a Bluetooth headset at 8 or 16 kHz, an interface at 96 or
/// 192 kHz, or a change of microphone halfway through all end up in the same 48 kHz mono file.
final class TrackWriter: @unchecked Sendable {
    static let bitRate = 64_000
    static let sampleRate: Double = 48_000
    let role: RecordedTrack.Role
    let url: URL
    private let format: AVAudioFormat
    private let file: Mutex<AVAudioFile?>
    private let queue: DispatchQueue
    private let currentLevel = Mutex<Float>(0)
    private let failure = Mutex<String?>(nil)
    /// Only touched on `queue`.
    private var resampler: Resampler?

    init(role: RecordedTrack.Role, url: URL) throws {
        self.role = role
        self.url = url
        format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: Self.sampleRate, channels: 1, interleaved: false)!
        let settings: [String: Any] = [
            AVFormatIDKey: kAudioFormatMPEG4AAC,
            AVSampleRateKey: Self.sampleRate,
            AVNumberOfChannelsKey: 1,
            // Constant 64 kbps mono: about 29 MB an hour, so a meeting of up to three hours fits
            // the 100 MB a space keeps in one file.
            AVEncoderBitRateKey: TrackWriter.bitRate,
            AVEncoderBitRateStrategyKey: AVAudioBitRateStrategy_Constant,
        ]
        do {
            file = Mutex(try AVAudioFile(forWriting: url, settings: settings, commonFormat: .pcmFormatFloat32, interleaved: false))
        } catch {
            throw RecordingError.writeFailed(error.localizedDescription)
        }
        queue = DispatchQueue(label: "md.knode.recording.\(role.rawValue)", qos: .userInitiated)
    }

    var level: Float { currentLevel.withLock { $0 } }

    /// Copies the buffer at once (it may belong to the audio thread) and writes it later.
    func append(_ buffer: AVAudioPCMBuffer) {
        guard let monoFormat = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: buffer.format.sampleRate, channels: 1, interleaved: false),
              let mono = AudioMixdown.mono(buffer, into: monoFormat) else { return }
        currentLevel.withLock { $0 = AudioMixdown.level(of: mono) }
        let box = BufferBox(buffer: mono)
        queue.async { [self] in
            write(box.buffer)
        }
    }

    /// Writes what is queued, what the resampler still holds, and closes the file, so its
    /// header is complete and it can be read.
    func finish() throws {
        queue.sync {
            drain()
        }
        file.withLock { file in
            file?.close()
            file = nil
        }
        if let reason = failure.withLock({ $0 }) { throw RecordingError.writeFailed(reason) }
    }

    private func write(_ mono: AVAudioPCMBuffer) {
        do {
            if mono.format.sampleRate == format.sampleRate {
                drain()
                try writeToFile(mono)
                return
            }
            if resampler?.inputRate != mono.format.sampleRate {
                drain()
                resampler = try Resampler(from: mono.format, to: format)
            }
            if let output = try resampler?.convert(mono) { try writeToFile(output) }
        } catch {
            failure.withLock { $0 = $0 ?? error.localizedDescription }
        }
    }

    /// Flushes the resampler of the input that just ended.
    private func drain() {
        guard let resampler else { return }
        self.resampler = nil
        do {
            if let tail = try resampler.finish() { try writeToFile(tail) }
        } catch {
            failure.withLock { $0 = $0 ?? error.localizedDescription }
        }
    }

    private func writeToFile(_ buffer: AVAudioPCMBuffer) throws {
        try file.withLock { file in
            try file?.write(from: buffer)
        }
    }
}

/// Mono float audio from one sample rate to another, keeping the converter's state between
/// buffers so a stream is resampled without clicks at the joins.
final class Resampler {
    let inputRate: Double
    private let converter: AVAudioConverter
    private let output: AVAudioFormat

    init(from input: AVAudioFormat, to output: AVAudioFormat) throws {
        guard let converter = AVAudioConverter(from: input, to: output) else {
            throw RecordingError.writeFailed("no converter from \(Int(input.sampleRate)) Hz to \(Int(output.sampleRate)) Hz")
        }
        converter.sampleRateConverterQuality = AVAudioQuality.high.rawValue
        self.converter = converter
        self.output = output
        inputRate = input.sampleRate
    }

    func convert(_ buffer: AVAudioPCMBuffer) throws -> AVAudioPCMBuffer? {
        try run(buffer)
    }

    /// What the converter still holds once the input has ended.
    func finish() throws -> AVAudioPCMBuffer? {
        try run(nil)
    }

    private func run(_ input: AVAudioPCMBuffer?) throws -> AVAudioPCMBuffer? {
        let frames = Double(input?.frameLength ?? 0) * output.sampleRate / inputRate
        guard let buffer = AVAudioPCMBuffer(pcmFormat: output, frameCapacity: AVAudioFrameCount(frames.rounded(.up)) + 2048) else { return nil }
        var fed = false
        var error: NSError?
        let status = converter.convert(to: buffer, error: &error) { _, status in
            guard let input else {
                status.pointee = .endOfStream
                return nil
            }
            if fed {
                status.pointee = .noDataNow
                return nil
            }
            fed = true
            status.pointee = .haveData
            return input
        }
        if status == .error { throw RecordingError.writeFailed(error?.localizedDescription ?? "the audio could not be resampled") }
        return buffer.frameLength > 0 ? buffer : nil
    }
}

/// Whether a recording is paused, read on the audio threads.
final class PauseFlag: Sendable {
    private let value = Atomic<Bool>(false)

    var isOn: Bool { value.load(ordering: .relaxed) }
    func set(_ on: Bool) { value.store(on, ordering: .relaxed) }
    /// Sets it and returns what it was.
    func exchange(_ on: Bool) -> Bool { value.exchange(on, ordering: .relaxed) }
}

private struct BufferBox: @unchecked Sendable {
    let buffer: AVAudioPCMBuffer
}

/// Mixing a buffer of any channel layout down to one channel, and its level.
enum AudioMixdown {
    static func mono(_ buffer: AVAudioPCMBuffer, into format: AVAudioFormat) -> AVAudioPCMBuffer? {
        let frames = Int(buffer.frameLength)
        let channels = Int(buffer.format.channelCount)
        guard frames > 0, channels > 0,
              let output = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(frames)),
              let out = output.floatChannelData?[0] else { return nil }
        output.frameLength = AVAudioFrameCount(frames)
        let interleaved = buffer.format.isInterleaved
        let scale = 1 / Float(channels)
        if let data = buffer.floatChannelData {
            for frame in 0..<frames {
                var sum: Float = 0
                for channel in 0..<channels {
                    sum += interleaved ? data[0][frame * channels + channel] : data[channel][frame]
                }
                out[frame] = sum * scale
            }
        } else if let data = buffer.int16ChannelData {
            for frame in 0..<frames {
                var sum: Float = 0
                for channel in 0..<channels {
                    sum += Float(interleaved ? data[0][frame * channels + channel] : data[channel][frame]) / Float(Int16.max)
                }
                out[frame] = sum * scale
            }
        } else {
            return nil
        }
        return output
    }

    /// The RMS level of a mono buffer, mapped from -50…0 dBFS to 0…1.
    static func level(of buffer: AVAudioPCMBuffer) -> Float {
        guard let data = buffer.floatChannelData?[0], buffer.frameLength > 0 else { return 0 }
        let count = Int(buffer.frameLength)
        var sum: Float = 0
        for index in 0..<count { sum += data[index] * data[index] }
        let rms = (sum / Float(count)).squareRoot()
        guard rms > 0 else { return 0 }
        let decibels = 20 * log10(rms)
        return min(max((decibels + 50) / 50, 0), 1)
    }
}

/// The system audio, through a Core Audio process tap on every app but Knode (or on chosen apps)
/// and a private aggregate device that reads it.
///
/// The aggregate holds the tap alone, as in Apple's "Capturing system audio with Core Audio taps",
/// so every input stream it delivers is the tap's. Only when such an aggregate cannot be made is
/// the default output device added as its clock; its input streams (a headset's microphone, say)
/// then come first in the buffer list, and only the tap's own buffers, the last ones, are read.
final class SystemAudioTap: @unchecked Sendable {
    private var tap: AudioHardwareTap?
    private var aggregate: AudioHardwareAggregateDevice?
    private var procID: AudioDeviceIOProcID?
    private var format: AVAudioFormat?
    private let queue = DispatchQueue(label: "md.knode.recording.tap", qos: .userInitiated)

    /// Creates the tap and its aggregate device, and returns the format the audio comes in.
    func prepare(source: SystemAudioSource) throws -> AVAudioFormat {
        let system = AudioHardwareSystem.shared
        let description: CATapDescription
        switch source {
        case .allApps:
            let own = (try? system.process(for: getpid())).flatMap { $0 }.map { [$0.id] } ?? []
            description = CATapDescription(stereoGlobalTapButExcludeProcesses: own)
        case .apps(let bundleIDs):
            let processes = ((try? system.processes) ?? []).filter { process in
                guard let id = (try? process.bundleID).flatMap({ $0 }) else { return false }
                return bundleIDs.contains(id)
            }
            guard !processes.isEmpty else { throw RecordingError.systemAudioUnavailable("the meeting app is not running") }
            description = CATapDescription(stereoMixdownOfProcesses: processes.map(\.id))
        }
        description.uuid = UUID()
        description.name = "Knode meeting recording"
        description.isPrivate = true
        description.muteBehavior = .unmuted

        do {
            guard let tap = try system.makeProcessTap(description: description) else {
                throw RecordingError.audioCapturePermissionDenied
            }
            self.tap = tap
            var streamDescription = try tap.format
            guard let format = AVAudioFormat(streamDescription: &streamDescription) else {
                throw RecordingError.systemAudioUnavailable("the audio format is not supported")
            }
            self.format = format
            let tapOnly = Self.composition(tap: description.uuid, clock: nil)
            var aggregate = try? system.makeAggregateDevice(description: tapOnly)
            if aggregate == nil, let output = try system.defaultOutputDevice, let uid = try? output.uid {
                aggregate = try system.makeAggregateDevice(description: Self.composition(tap: description.uuid, clock: uid))
            }
            guard let aggregate else {
                throw RecordingError.systemAudioUnavailable("the aggregate device could not be made")
            }
            self.aggregate = aggregate
            return format
        } catch let error as RecordingError {
            stop()
            throw error
        } catch {
            stop()
            throw RecordingError.systemAudioUnavailable(error.localizedDescription)
        }
    }

    /// A private aggregate device of the tap, with `clock` (an output device's UID) as its time
    /// source when given.
    static func composition(tap: UUID, clock: String?) -> [String: Any] {
        var composition: [String: Any] = [
            kAudioAggregateDeviceNameKey: "Knode Meeting Recording",
            kAudioAggregateDeviceUIDKey: UUID().uuidString,
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceIsStackedKey: false,
            kAudioAggregateDeviceTapAutoStartKey: true,
            kAudioAggregateDeviceTapListKey: [[
                kAudioSubTapUIDKey: tap.uuidString,
                kAudioSubTapDriftCompensationKey: true,
            ]],
        ]
        if let clock {
            composition[kAudioAggregateDeviceMainSubDeviceKey] = clock
            composition[kAudioAggregateDeviceSubDeviceListKey] = [[kAudioSubDeviceUIDKey: clock]]
        }
        return composition
    }

    /// Starts reading; each buffer is only valid during the call.
    func start(_ onBuffer: @escaping @Sendable (AVAudioPCMBuffer) -> Void) throws {
        guard let aggregate, let format else { throw RecordingError.systemAudioUnavailable("the tap is not prepared") }
        let selector = TapBufferSelector(format: format)
        var procID: AudioDeviceIOProcID?
        let status = AudioDeviceCreateIOProcIDWithBlock(&procID, aggregate.id, queue) { _, input, _, _, _ in
            guard let list = selector.tapBuffers(of: input),
                  let buffer = AVAudioPCMBuffer(pcmFormat: selector.format, bufferListNoCopy: list, deallocator: nil) else { return }
            onBuffer(buffer)
        }
        guard status == noErr, let procID else {
            stop()
            throw RecordingError.systemAudioUnavailable("Core Audio error \(status)")
        }
        self.procID = procID
        do {
            try aggregate.start(IOProcID: procID)
        } catch {
            stop()
            throw RecordingError.audioCapturePermissionDenied
        }
    }

    func stop() {
        let system = AudioHardwareSystem.shared
        if let aggregate {
            if let procID {
                try? aggregate.stop(IOProcID: procID)
                AudioDeviceDestroyIOProcID(aggregate.id, procID)
            }
            try? system.destroyAggregateDevice(aggregate)
        }
        if let tap { try? system.destroyProcessTap(tap) }
        procID = nil
        aggregate = nil
        tap = nil
    }
}

/// Picks the tap's buffers out of what the aggregate device delivers: all of them when it holds
/// the tap alone, else the last ones, as many as the tap's format has (one when interleaved, one
/// a channel when not). Called on the tap's serial queue only.
final class TapBufferSelector: @unchecked Sendable {
    let format: AVAudioFormat
    let tapBufferCount: Int
    private let scratch: UnsafeMutableAudioBufferListPointer

    init(format: AVAudioFormat) {
        self.format = format
        tapBufferCount = format.isInterleaved ? 1 : Int(format.channelCount)
        scratch = AudioBufferList.allocate(maximumBuffers: max(tapBufferCount, 1))
    }

    deinit {
        free(scratch.unsafeMutablePointer)
    }

    /// The tap's buffers, or nil when the list has fewer than the tap needs.
    func tapBuffers(of input: UnsafePointer<AudioBufferList>) -> UnsafePointer<AudioBufferList>? {
        let all = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: input))
        guard let range = Self.tapRange(bufferCount: all.count, tapBufferCount: tapBufferCount) else { return nil }
        if range.count == all.count { return input }
        for (index, source) in range.enumerated() { scratch[index] = all[source] }
        return UnsafePointer(scratch.unsafePointer)
    }

    /// Which buffers of `bufferCount` are the tap's: the trailing `tapBufferCount`.
    static func tapRange(bufferCount: Int, tapBufferCount: Int) -> Range<Int>? {
        guard tapBufferCount > 0, bufferCount >= tapBufferCount else { return nil }
        return (bufferCount - tapBufferCount)..<bufferCount
    }
}
