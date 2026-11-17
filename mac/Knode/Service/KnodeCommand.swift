import Foundation
import os

/// Runs the `knode` command, for `knode service start|stop|install|uninstall`.
struct KnodeCommand: Sendable {
    var executable: URL
    /// The environment to run it in; this process's when nil.
    var environment: [String: String]? = nil

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
        if let environment { process.environment = environment }
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

extension KnodeCommand {
    /// The `knode` command the app runs, in the environment a terminal session has: the folders a
    /// shell finds commands in on its `PATH`, so `knode connect` finds `claude` and `codex`, and
    /// the variables that say where agents keep their settings (`CLAUDE_CONFIG_DIR`,
    /// `CODEX_HOME`, ...), read from the person's login shell, so it configures the folder their
    /// sessions read. Apps opened from the Finder start with neither.
    static func withShellPath() async -> KnodeCommand {
        let shell = await ShellEnvironment.shared.values()
        let environment = withShellPath(shell: shell, process: ProcessInfo.processInfo.environment, userHome: FileManager.default.homeDirectoryForCurrentUser)
        return KnodeCommand(executable: CommandLineTool.forThisApp.executable(), environment: environment)
    }

    /// This process's environment with the login shell's `PATH` and agent settings variables.
    /// The shell's value wins: it is what a terminal session, and so an agent, sees.
    static func withShellPath(shell: [String: String], process: [String: String], userHome: URL) -> [String: String] {
        var environment = process
        let path = [shell["PATH"], process["PATH"]].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: ":")
        environment["PATH"] = CommandLineTool.searchDirectories(path: path, userHome: userHome)
            .map { $0.path(percentEncoded: false) }
            .joined(separator: ":")
        for name in ShellEnvironment.agentVariables {
            if let value = shell[name], !value.isEmpty { environment[name] = value }
        }
        return environment
    }
}

/// The environment of the person's login shell, read once: an app opened from the Finder does not
/// get what their shell startup files set.
actor ShellEnvironment {
    static let shared = ShellEnvironment()

    /// The variables `knode connect` reads to find where an agent keeps its settings.
    static let agentVariables = ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "COPILOT_HOME", "CLINE_DIR", "CLINE_MCP_SETTINGS_PATH", "XDG_CONFIG_HOME"]
    /// Printed around the environment, to tell it apart from what startup files print.
    static let marker = "__KNODE_SHELL_ENVIRONMENT__"

    private var cached: [String: String]?

    func values() async -> [String: String] {
        if let cached { return cached }
        let values = await Self.read(shell: Self.loginShell())
        cached = values
        return values
    }

    /// The person's shell: `SHELL`, else zsh.
    static func loginShell(environment: [String: String] = ProcessInfo.processInfo.environment) -> URL {
        if let shell = environment["SHELL"], shell.hasPrefix("/") { return URL(filePath: shell) }
        return URL(filePath: "/bin/zsh")
    }

    /// Runs `shell` as an interactive login shell, as a terminal does, and returns its `PATH` and
    /// agent settings variables. Empty when it fails or takes longer than `timeout`.
    static func read(shell: URL, timeout: Duration = .seconds(5)) async -> [String: String] {
        let process = Process()
        process.executableURL = shell
        process.arguments = ["-l", "-i", "-c", "echo \(marker); /usr/bin/env -0; echo \(marker)"]
        let outputURL = FileManager.default.temporaryDirectory.appending(path: "knode-shell-\(UUID().uuidString).log")
        FileManager.default.createFile(atPath: outputURL.path(percentEncoded: false), contents: nil)
        defer { try? FileManager.default.removeItem(at: outputURL) }
        guard let output = try? FileHandle(forWritingTo: outputURL) else { return [:] }
        defer { try? output.close() }
        process.standardOutput = output
        process.standardError = FileHandle.nullDevice
        process.standardInput = FileHandle.nullDevice
        let finished: Bool = await withCheckedContinuation { continuation in
            let resumed = OSAllocatedUnfairLock(initialState: false)
            let resume: @Sendable (Bool) -> Void = { value in
                let first = resumed.withLock { done in
                    defer { done = true }
                    return !done
                }
                if first { continuation.resume(returning: value) }
            }
            process.terminationHandler = { _ in resume(true) }
            do {
                try process.run()
            } catch {
                resume(false)
                return
            }
            Task {
                try? await Task.sleep(for: timeout)
                if process.isRunning { process.terminate() }
                resume(false)
            }
        }
        guard finished else { return [:] }
        return parse(String(decoding: (try? Data(contentsOf: outputURL)) ?? Data(), as: UTF8.self))
    }

    /// The `PATH` and agent settings variables in what `read` printed, ignoring anything startup
    /// files printed around it.
    static func parse(_ output: String) -> [String: String] {
        guard let start = output.range(of: marker), let end = output.range(of: marker, options: .backwards),
              start.upperBound <= end.lowerBound
        else { return [:] }
        let wanted = Set(agentVariables + ["PATH"])
        var values: [String: String] = [:]
        for entry in output[start.upperBound..<end.lowerBound].split(separator: "\0") {
            let entry = entry.drop { $0 == "\n" }
            guard let equals = entry.firstIndex(of: "=") else { continue }
            let name = String(entry[..<equals])
            if wanted.contains(name) { values[name] = String(entry[entry.index(after: equals)...]) }
        }
        return values
    }
}
