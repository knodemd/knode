import Darwin
import Foundation
import Synchronization

/// What can go wrong talking to Knode Service.
public enum ServiceError: Error, Equatable, LocalizedError {
    /// Nothing listens on the socket: the service is not running.
    case unavailable(String)
    /// The service closed the connection, or it was closed here.
    case disconnected
    /// The service answered with a JSON-RPC error. `kind` is its stable `data.kind`.
    case remote(code: Int, kind: String?, message: String)
    /// The answer could not be read.
    case badResponse(String)

    public var errorDescription: String? {
        switch self {
        case .unavailable: "Knode Service is not running."
        case .disconnected: "Knode Service stopped."
        case .remote(_, _, let message): message.prefix(1).uppercased() + message.dropFirst() + "."
        case .badResponse(let message): "Knode Service sent an answer the app can't read: \(message)."
        }
    }
}

/// One connection to Knode Service: JSON-RPC 2.0 over its Unix socket, one message per line
/// (`core/crates/knode-service/PROTOCOL.md`).
///
/// Requests may be sent from any task; the service answers them in order. Notifications, such as
/// `changed`, arrive on `notifications`, which ends when the connection closes.
public final class ServiceConnection: Sendable {
    /// A notification from the service: its method and the whole JSON line it came in.
    public struct Notification: Sendable {
        public var method: String
        public var line: Data

        /// Decodes `params` as `Params`.
        public func params<Params: Decodable>(as type: Params.Type) throws -> Params {
            try ServiceConnection.decoder.decode(Envelope<Params>.self, from: line).params
        }

        private struct Envelope<P: Decodable>: Decodable { var params: P }
    }

    private struct State {
        var nextID = 1
        var pending: [Int: CheckedContinuation<Data, any Error>] = [:]
        var closed = false
    }

    public let notifications: AsyncStream<Notification>
    private let notify: AsyncStream<Notification>.Continuation
    private let socket: Int32
    private let state = Mutex(State())
    private let writing = Mutex(())

    static let encoder = JSONEncoder()
    static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()

    /// Connects to the socket at `url`. Throws `ServiceError.unavailable` when nothing listens.
    public static func connect(to url: URL) throws -> ServiceConnection {
        let path = url.path(percentEncoded: false)
        let socket = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard socket >= 0 else { throw ServiceError.unavailable(String(cString: strerror(errno))) }
        var on: Int32 = 1
        setsockopt(socket, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))

        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8) + [0]
        guard bytes.count <= MemoryLayout.size(ofValue: address.sun_path) else {
            Darwin.close(socket)
            throw ServiceError.unavailable("the socket path is too long")
        }
        withUnsafeMutableBytes(of: &address.sun_path) { $0.copyBytes(from: bytes) }
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        let result = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(socket, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard result == 0 else {
            let message = String(cString: strerror(errno))
            Darwin.close(socket)
            throw ServiceError.unavailable(message)
        }
        return ServiceConnection(socket: socket)
    }

    private init(socket: Int32) {
        self.socket = socket
        (notifications, notify) = AsyncStream.makeStream(bufferingPolicy: .bufferingNewest(4096))
        let thread = Thread { [self] in readLoop() }
        thread.name = "md.knode.service-connection"
        thread.start()
    }

    deinit {
        Darwin.close(socket)
    }

    /// True once the connection has closed.
    public var isClosed: Bool { state.withLock { $0.closed } }

    /// Closes the connection. Requests still waiting fail with `ServiceError.disconnected`.
    public func close() {
        shutdown(socket, SHUT_RDWR)
    }

    /// Sends `method` with `params` and waits for its result, decoded as `Result`. Use an optional
    /// `Result` for methods that may answer `null`.
    public func call<Result: Decodable>(_ method: String, _ params: [String: JSONValue] = [:], as type: Result.Type = Result.self) async throws -> Result {
        let line = try await send(method, params)
        let response: Response<Result>
        do {
            response = try Self.decoder.decode(Response<Result>.self, from: line)
        } catch {
            throw ServiceError.badResponse(String(describing: error))
        }
        if let error = response.error {
            throw ServiceError.remote(code: error.code, kind: error.data?.kind, message: error.message)
        }
        guard let result = response.result else { throw ServiceError.badResponse("no result") }
        return result
    }

    private func send(_ method: String, _ params: [String: JSONValue]) async throws -> Data {
        try await withCheckedThrowingContinuation { continuation in
            let id: Int? = state.withLock { state in
                guard !state.closed else { return nil }
                let id = state.nextID
                state.nextID += 1
                state.pending[id] = continuation
                return id
            }
            guard let id else {
                continuation.resume(throwing: ServiceError.disconnected)
                return
            }
            do {
                var data = try Self.encoder.encode(Request(id: id, method: method, params: .object(params)))
                data.append(0x0A)
                try write(data)
            } catch {
                // Only fail the request if the reader has not answered or failed it already.
                if let waiting = state.withLock({ $0.pending.removeValue(forKey: id) }) {
                    waiting.resume(throwing: error is ServiceError ? error : ServiceError.badResponse(String(describing: error)))
                }
            }
        }
    }

    private func write(_ data: Data) throws {
        try writing.withLock { _ in
            try data.withUnsafeBytes { buffer in
                var offset = 0
                while offset < buffer.count {
                    let written = Darwin.write(socket, buffer.baseAddress! + offset, buffer.count - offset)
                    if written < 0 {
                        if errno == EINTR { continue }
                        throw ServiceError.disconnected
                    }
                    offset += written
                }
            }
        }
    }

    // MARK: Reading

    private func readLoop() {
        var buffer = Data()
        var chunk = [UInt8](repeating: 0, count: 64 * 1024)
        while true {
            let count = chunk.withUnsafeMutableBytes { Darwin.read(socket, $0.baseAddress, $0.count) }
            if count < 0, errno == EINTR { continue }
            guard count > 0 else { break }
            buffer.append(contentsOf: chunk[0..<count])
            while let newline = buffer.firstIndex(of: 0x0A) {
                let line = Data(buffer[buffer.startIndex..<newline])
                buffer = Data(buffer[buffer.index(after: newline)...])
                if !line.isEmpty { handle(line) }
            }
        }
        let waiting = state.withLock { state in
            state.closed = true
            defer { state.pending = [:] }
            return Array(state.pending.values)
        }
        for continuation in waiting { continuation.resume(throwing: ServiceError.disconnected) }
        notify.finish()
    }

    private func handle(_ line: Data) {
        guard let peek = try? Self.decoder.decode(Peek.self, from: line) else { return }
        if let id = peek.id {
            state.withLock { $0.pending.removeValue(forKey: id) }?.resume(returning: line)
        } else if let method = peek.method {
            notify.yield(Notification(method: method, line: line))
        }
    }

    // MARK: Messages

    private struct Request: Encodable {
        var jsonrpc = "2.0"
        var id: Int
        var method: String
        var params: JSONValue
    }

    private struct Peek: Decodable {
        var id: Int?
        var method: String?
    }

    private struct Response<Result: Decodable>: Decodable {
        var result: Result?
        var error: RPCError?

        enum CodingKeys: String, CodingKey { case result, error }

        init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            error = try container.decodeIfPresent(RPCError.self, forKey: .error)
            // `decode`, not `decodeIfPresent`: an optional `Result` takes `null` as `nil`.
            result = error == nil ? try container.decode(Result.self, forKey: .result) : nil
        }
    }

    private struct RPCError: Decodable {
        var code: Int
        var message: String
        var data: Details?

        struct Details: Decodable { var kind: String? }
    }
}
