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
        }
    }
}

/// Records a meeting's audio into files. `MeetingRecorder` is the real one; tests use fakes.
protocol MeetingRecording: AnyObject, Sendable {
    /// Starts recording, asking for the microphone permission the first time.
    func start(_ mode: RecordingMode) async throws
    func pause()
    func resume()
    /// Stops and returns the finished files.
    func stop() async throws -> [RecordedTrack]
    /// Stops and throws the files away.
    func cancel()
    /// How long it has recorded, pauses left out.
    var elapsed: Duration { get }
    /// The loudest track's level now, from 0 to 1, for a meter.
    var level: Float { get }
    var isPaused: Bool { get }
}

/// Records the microphone with `AVAudioEngine` and, for an online meeting, the system audio through
/// a Core Audio process tap, each into its own AAC file (`microphone.m4a`, `system.m4a`) in a
/// private temporary folder. In person, only the microphone is recorded (`mixed`).
///
/// The system audio permission has no status to read: macOS asks the first time the tap starts,
/// and when it is refused the tap delivers silence.
final class MeetingRecorder: MeetingRecording, @unchecked Sendable {
    private struct Session {
        var folder: URL
        var engine: AVAudioEngine
        var microphone: TrackWriter
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
            return max(session.microphone.level, session.system?.level ?? 0)
        }
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
        let engine = AVAudioEngine()
        let input = engine.inputNode
        let inputFormat = input.outputFormat(forBus: 0)
        guard inputFormat.channelCount > 0, inputFormat.sampleRate > 0 else { throw RecordingError.noInputDevice }

        let role: RecordedTrack.Role = mode == .inPerson ? .mixed : .microphone
        let microphone = try TrackWriter(role: role, url: folder.appending(path: "microphone.m4a"), sampleRate: inputFormat.sampleRate)
        var system: TrackWriter?
        var tap: SystemAudioTap?
        if case .online(let source) = mode {
            let systemTap = SystemAudioTap()
            let format = try systemTap.prepare(source: source)
            let writer: TrackWriter
            do {
                writer = try TrackWriter(role: .system, url: folder.appending(path: "system.m4a"), sampleRate: format.sampleRate)
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

        let paused = self.paused
        input.installTap(onBus: 0, bufferSize: 4096, format: inputFormat) { buffer, _ in
            guard !paused.isOn else { return }
            microphone.append(buffer)
        }
        do {
            engine.prepare()
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            tap?.stop()
            throw RecordingError.noInputDevice
        }
        return Session(folder: folder, engine: engine, microphone: microphone, system: system, tap: tap, mode: mode)
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
        session.engine.inputNode.removeTap(onBus: 0)
        session.engine.stop()
        session.tap?.stop()
        var tracks: [RecordedTrack] = []
        for writer in [session.microphone, session.system].compactMap({ $0 }) {
            try writer.finish()
            tracks.append(RecordedTrack(role: writer.role, url: writer.url))
        }
        return tracks
    }

    func cancel() {
        guard let session = state.withLock({ session in defer { session = nil }; return session }) else { return }
        pause()
        session.engine.inputNode.removeTap(onBus: 0)
        session.engine.stop()
        session.tap?.stop()
        try? session.microphone.finish()
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

/// Writes one track: mixes each buffer down to mono and encodes it to AAC on its own queue, off the
/// audio thread, and keeps the level of the latest buffer for the meter.
final class TrackWriter: @unchecked Sendable {
    let role: RecordedTrack.Role
    let url: URL
    private let format: AVAudioFormat
    private let file: Mutex<AVAudioFile?>
    private let queue: DispatchQueue
    private let currentLevel = Mutex<Float>(0)
    private let failure = Mutex<String?>(nil)

    init(role: RecordedTrack.Role, url: URL, sampleRate: Double) throws {
        self.role = role
        self.url = url
        guard let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: sampleRate, channels: 1, interleaved: false) else {
            throw RecordingError.writeFailed("no audio format at \(Int(sampleRate)) Hz")
        }
        self.format = format
        let settings: [String: Any] = [
            AVFormatIDKey: kAudioFormatMPEG4AAC,
            AVSampleRateKey: sampleRate,
            AVNumberOfChannelsKey: 1,
            AVEncoderBitRateKey: 64_000,
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
        guard let mono = AudioMixdown.mono(buffer, into: format) else { return }
        currentLevel.withLock { $0 = AudioMixdown.level(of: mono) }
        let box = BufferBox(buffer: mono)
        queue.async { [self] in
            file.withLock { file in
                do {
                    try file?.write(from: box.buffer)
                } catch {
                    failure.withLock { $0 = $0 ?? error.localizedDescription }
                }
            }
        }
    }

    /// Writes what is queued and closes the file.
    func finish() throws {
        queue.sync {}
        file.withLock { file in
            file?.close()
            file = nil
        }
        if let reason = failure.withLock({ $0 }) { throw RecordingError.writeFailed(reason) }
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
            var composition: [String: Any] = [
                kAudioAggregateDeviceNameKey: "Knode Meeting Recording",
                kAudioAggregateDeviceUIDKey: UUID().uuidString,
                kAudioAggregateDeviceIsPrivateKey: true,
                kAudioAggregateDeviceIsStackedKey: false,
                kAudioAggregateDeviceTapAutoStartKey: true,
                kAudioAggregateDeviceTapListKey: [[
                    kAudioSubTapUIDKey: description.uuid.uuidString,
                    kAudioSubTapDriftCompensationKey: true,
                ]],
            ]
            if let output = try system.defaultOutputDevice, let uid = try? output.uid {
                composition[kAudioAggregateDeviceMainSubDeviceKey] = uid
                composition[kAudioAggregateDeviceSubDeviceListKey] = [[kAudioSubDeviceUIDKey: uid]]
            }
            guard let aggregate = try system.makeAggregateDevice(description: composition) else {
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

    /// Starts reading; each buffer is only valid during the call.
    func start(_ onBuffer: @escaping @Sendable (AVAudioPCMBuffer) -> Void) throws {
        guard let aggregate, let format else { throw RecordingError.systemAudioUnavailable("the tap is not prepared") }
        let box = FormatBox(format: format)
        var procID: AudioDeviceIOProcID?
        let status = AudioDeviceCreateIOProcIDWithBlock(&procID, aggregate.id, queue) { _, input, _, _, _ in
            guard let buffer = AVAudioPCMBuffer(pcmFormat: box.format, bufferListNoCopy: input, deallocator: nil) else { return }
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

private struct FormatBox: @unchecked Sendable {
    let format: AVAudioFormat
}
