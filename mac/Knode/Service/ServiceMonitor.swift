import Foundation
import Observation

/// What the menu bar shows about Knode Service, kept up to date over its socket.
///
/// While the service runs, the monitor holds a connection subscribed to every change and asks for
/// `service.status` after each change and every few seconds (clients come and go without a
/// change). While it does not, the monitor tries to connect every few seconds.
@MainActor
@Observable
final class ServiceMonitor {
    struct Status: Decodable, Equatable, Sendable {
        var version: String
        var pid: Int
        /// The paths of the spaces the service has open.
        var spaces: [String]
        /// The connections open to the service, this app's included.
        var clients: Int
        /// Search by meaning, as the index last saw it; `nil` from a service before it.
        var index: IndexStatus?
    }

    enum State: Equatable {
        case checking
        case running(Status)
        case stopped
    }

    private(set) var state = State.checking
    /// Whether the service starts at login (its launch agent is installed).
    private(set) var runsAtLogin = false
    /// A `knode service` command is running.
    private(set) var isBusy = false
    var errorMessage: String?

    var isRunning: Bool {
        if case .running = state { true } else { false }
    }

    /// Called when the service starts running or stops.
    @ObservationIgnored var onRunningChange: (@MainActor (Bool) -> Void)?

    @ObservationIgnored private let socketURL: URL
    @ObservationIgnored private let launchAgentURL: URL
    @ObservationIgnored private let command: @MainActor () -> KnodeCommand
    @ObservationIgnored private let pollInterval: Duration
    @ObservationIgnored private var task: Task<Void, Never>?

    init(
        socketURL: URL = KnodeHome.current.socketURL,
        launchAgentURL: URL = ServiceMonitor.defaultLaunchAgentURL,
        pollInterval: Duration = .seconds(3),
        command: @escaping @MainActor () -> KnodeCommand
    ) {
        self.socketURL = socketURL
        self.launchAgentURL = launchAgentURL
        self.pollInterval = pollInterval
        self.command = command
    }

    /// `~/Library/LaunchAgents/md.knode.service.plist`, written by `knode service install`.
    nonisolated static var defaultLaunchAgentURL: URL {
        FileManager.default.homeDirectoryForCurrentUser.appending(path: "Library/LaunchAgents/md.knode.service.plist")
    }

    func start() {
        guard task == nil else { return }
        refreshRunsAtLogin()
        task = Task { [weak self] in await self?.watch() }
    }

    func stop() {
        task?.cancel()
        task = nil
    }

    func refreshRunsAtLogin() {
        runsAtLogin = FileManager.default.fileExists(atPath: launchAgentURL.path(percentEncoded: false))
    }

    // MARK: Actions

    func startService() async { await perform(["service", "start"]) }

    func stopService() async { await perform(["service", "stop"]) }

    func setRunsAtLogin(_ on: Bool) async {
        await perform(["service", on ? "install" : "uninstall"])
        refreshRunsAtLogin()
    }

    private func perform(_ arguments: [String]) async {
        isBusy = true
        defer { isBusy = false }
        do {
            try await command().run(arguments)
            errorMessage = nil
        } catch {
            // The first line fits in a menu; the command prints help after it.
            errorMessage = error.localizedDescription.split(separator: "\n").first.map(String.init)
        }
        // Show the new state at once rather than at the next poll.
        task?.cancel()
        task = Task { [weak self] in await self?.watch() }
    }

    // MARK: Watching

    private func watch() async {
        while !Task.isCancelled {
            if let connection = try? ServiceConnection.connect(to: socketURL) {
                await follow(connection)
                connection.close()
            } else {
                setState(.stopped)
            }
            try? await Task.sleep(for: pollInterval)
        }
    }

    /// Follows a connection until it closes or the task is cancelled.
    private func follow(_ connection: ServiceConnection) async {
        guard (try? await connection.call("subscribe", as: Bool.self)) != nil else { return }
        let notifications = connection.notifications
        let refresh = AsyncStream<Void>.makeStream(bufferingPolicy: .bufferingNewest(1))
        // Each change asks for a new status; the timer asks too.
        let listener = Task {
            for await _ in notifications { refresh.continuation.yield() }
            refresh.continuation.finish()
        }
        let interval = pollInterval
        let timer = Task {
            while !Task.isCancelled {
                refresh.continuation.yield()
                try? await Task.sleep(for: interval)
            }
        }
        defer {
            listener.cancel()
            timer.cancel()
        }
        for await _ in refresh.stream {
            guard !Task.isCancelled else { break }
            guard let status = try? await connection.call("service.status", as: Status.self) else { break }
            setState(.running(status))
        }
        if !Task.isCancelled { setState(.stopped) }
    }

    private func setState(_ new: State) {
        let wasRunning = isRunning
        if state != new { state = new }
        if isRunning != wasRunning { onRunningChange?(isRunning) }
    }
}
