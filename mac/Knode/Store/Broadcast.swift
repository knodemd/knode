import Foundation
import Synchronization

/// Sends each value to every stream made by `stream()`, for as long as the stream is iterated.
final class Broadcast<Value: Sendable>: Sendable {
    private let continuations = Mutex<[UUID: AsyncStream<Value>.Continuation]>([:])

    func stream() -> AsyncStream<Value> {
        let (stream, continuation) = AsyncStream.makeStream(of: Value.self, bufferingPolicy: .bufferingNewest(4096))
        let id = UUID()
        continuation.onTermination = { [weak self] _ in
            _ = self?.continuations.withLock { $0.removeValue(forKey: id) }
        }
        continuations.withLock { $0[id] = continuation }
        return stream
    }

    func yield(_ value: Value) {
        for continuation in continuations.withLock({ Array($0.values) }) {
            continuation.yield(value)
        }
    }
}
