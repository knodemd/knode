import Foundation

/// Runs the `knode` command, for `knode service start|stop|install|uninstall`.
struct KnodeCommand: Sendable {
    var executable: URL

    struct Failure: Error, LocalizedError {
        var arguments: [String]
        var status: Int32
        var output: String

        var errorDescription: String? {
            let detail = output.trimmingCharacters(in: .whitespacesAndNewlines)
            return detail.isEmpty ? "knode \(arguments.joined(separator: " ")) failed (exit status \(status))." : detail
        }
    }

    /// Runs `knode <arguments>` and returns what it printed. Throws `Failure` if it fails.
    @discardableResult
    func run(_ arguments: [String]) async throws -> String {
        let process = Process()
        process.executableURL = executable
        process.arguments = arguments
        // A file, not a pipe: `knode service start` leaves the service running in the background,
        // which may keep a pipe open long after the command ends.
        let outputURL = FileManager.default.temporaryDirectory.appending(path: "knode-\(UUID().uuidString).log")
        FileManager.default.createFile(atPath: outputURL.path(percentEncoded: false), contents: nil)
        defer { try? FileManager.default.removeItem(at: outputURL) }
        let output = try FileHandle(forWritingTo: outputURL)
        defer { try? output.close() }
        process.standardOutput = output
        process.standardError = output
        process.standardInput = FileHandle.nullDevice
        let status: Int32 = try await withCheckedThrowingContinuation { continuation in
            process.terminationHandler = { continuation.resume(returning: $0.terminationStatus) }
            do {
                try process.run()
            } catch {
                process.terminationHandler = nil
                continuation.resume(throwing: error)
            }
        }
        let text = String(decoding: (try? Data(contentsOf: outputURL)) ?? Data(), as: UTF8.self)
        guard status == 0 else { throw Failure(arguments: arguments, status: status, output: text) }
        return text
    }
}
