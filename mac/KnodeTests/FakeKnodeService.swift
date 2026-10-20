import Darwin
import Foundation
import Synchronization

/// A stand-in for Knode Service in tests: it listens on a Unix socket, speaks the JSON-RPC protocol
/// of `PROTOCOL.md` and keeps spaces and nodes in memory.
final class FakeKnodeService: @unchecked Sendable {
    struct StoredNode {
        var id: String
        var title: String
        var body: String
        var createdAt: Int64
        var updatedAt: Int64
        var by: [String: Any]

        var json: [String: Any] {
            ["id": id, "kind": "note", "title": title, "body": body, "tags": [String](),
             "created_at": createdAt, "updated_at": updatedAt, "created_by": by, "updated_by": by]
        }
    }

    struct Space {
        var id: String
        var name: String
        var nodes: [String: StoredNode] = [:]
    }

    let socketURL: URL
    private let folder: URL
    private let listener: Int32
    private let lock = NSLock()
    private var spaces: [String: Space] = [:]
    private var clients: [Int32: Bool] = [:]  // socket: subscribed
    private var clock: Int64 = 1_792_489_800_000
    private var counter = 0
    private(set) var requests: [String] = []
    /// The params of the last request of each method.
    private(set) var lastParams: [String: [String: Any]] = [:]
    /// What `changes` answers for each subject, newest first.
    private var changeLog: [String: [[String: Any]]] = [:]
    /// Changes whose undo fails with `undo_conflict` unless forced, with their later changes.
    private var conflicts: [Int64: [Int64]] = [:]
    /// The agents `agents.list` answers with.
    private var agents: [[String: Any]] = []

    init() throws {
        // Unix socket paths are short: keep the folder name short.
        folder = URL(filePath: "/tmp/kn-\(UUID().uuidString.prefix(8))", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        socketURL = folder.appending(path: "service.sock")

        listener = socket(AF_UNIX, SOCK_STREAM, 0)
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(socketURL.path(percentEncoded: false).utf8) + [0]
        withUnsafeMutableBytes(of: &address.sun_path) { $0.copyBytes(from: bytes) }
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(listener, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard bound == 0, listen(listener, 16) == 0 else { throw POSIXError(.EADDRINUSE) }
        let thread = Thread { [self] in acceptLoop() }
        thread.start()
    }

    /// Stops the service: closes every connection and removes the socket.
    func stop() {
        shutdown(listener, SHUT_RDWR)
        close(listener)
        let sockets = lock.withLock { Array(clients.keys) }
        for socket in sockets { shutdown(socket, SHUT_RDWR) }
        try? FileManager.default.removeItem(at: folder)
    }

    /// Makes a change as an agent would, through another client.
    @discardableResult
    func agentCreatesNode(in path: String, title: String, body: String) -> String {
        let id = lock.withLock { createNode(in: path, title: title, body: body, by: ["kind": "agent", "actor": "claude-code"]) }
        notify(space: path, node: id, op: "create")
        return id
    }

    func agentUpdatesNode(_ id: String, in path: String, body: String) {
        lock.withLock {
            clock += 1
            spaces[path]?.nodes[id]?.body = body
            spaces[path]?.nodes[id]?.updatedAt = clock
        }
        notify(space: path, node: id, op: "update")
    }

    func node(_ id: String, in path: String) -> StoredNode? {
        lock.withLock { spaces[path]?.nodes[id] }
    }

    func params(of method: String) -> [String: Any]? {
        lock.withLock { lastParams[method] }
    }

    /// Sets what `changes` answers for a subject, newest first.
    func setChanges(_ changes: [[String: Any]], for subject: String) {
        lock.withLock { changeLog[subject] = changes }
    }

    /// Makes the undo of a change fail with `undo_conflict` unless it is forced.
    func setConflict(undoing change: Int64, later: [Int64]) {
        lock.withLock { conflicts[change] = later }
    }

    /// Adds an agent with one valid key and the given levels by space path.
    func addAgent(_ name: String, levels: [String: String]) {
        lock.withLock {
            agents.append([
                "id": nextID(), "name": name, "kind": name, "created_at": clock,
                "keys": [["fingerprint": "sha256:2d8e7ee0d1fd2f7fa95d62fff7405c54a6c86833c447fe84c53f2de800dfb3a1", "created_at": clock, "revoked_at": NSNull()]],
                "spaces": levels.sorted { $0.key < $1.key }.map { ["space": $0.key, "level": $0.value] },
            ])
        }
    }

    // MARK: Serving

    private func acceptLoop() {
        while true {
            let client = accept(listener, nil, nil)
            guard client >= 0 else { return }
            var on: Int32 = 1
            setsockopt(client, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
            lock.withLock { clients[client] = false }
            Thread { [self] in serve(client) }.start()
        }
    }

    private func serve(_ client: Int32) {
        var buffer = Data()
        var chunk = [UInt8](repeating: 0, count: 65536)
        while true {
            let count = chunk.withUnsafeMutableBytes { read(client, $0.baseAddress, $0.count) }
            guard count > 0 else { break }
            buffer.append(contentsOf: chunk[0..<count])
            while let newline = buffer.firstIndex(of: 0x0A) {
                let line = Data(buffer[buffer.startIndex..<newline])
                buffer = Data(buffer[buffer.index(after: newline)...])
                handle(line, from: client)
            }
        }
        lock.withLock { clients[client] = nil }
        close(client)
    }

    private func handle(_ line: Data, from client: Int32) {
        guard let request = try? JSONSerialization.jsonObject(with: line) as? [String: Any],
              let method = request["method"] as? String
        else { return }
        let id = request["id"]
        let params = request["params"] as? [String: Any] ?? [:]
        var notification: (space: String, node: String, op: String)?
        let answer: [String: Any] = lock.withLock {
            requests.append(method)
            lastParams[method] = params
            do {
                let (result, change) = try respond(method, params, client: client)
                notification = change
                return ["jsonrpc": "2.0", "id": id ?? NSNull(), "result": result]
            } catch let error as RPCError {
                let data = error.data.merging(["kind": error.kind]) { $1 }
                return ["jsonrpc": "2.0", "id": id ?? NSNull(), "error": ["code": error.code, "message": error.message, "data": data]]
            } catch {
                return ["jsonrpc": "2.0", "id": id ?? NSNull(), "error": ["code": -32603, "message": "\(error)"]]
            }
        }
        send(answer, to: client)
        if let notification { notify(space: notification.space, node: notification.node, op: notification.op) }
    }

    struct RPCError: Error {
        var code: Int
        var kind: String
        var message: String
        var data: [String: Any] = [:]
    }

    /// Answers one request; the caller holds the lock.
    private func respond(_ method: String, _ params: [String: Any], client: Int32) throws -> (Any, (String, String, String)?) {
        let path = params["space"] as? String ?? ""
        func space() throws -> Space {
            guard let space = spaces[path] else { throw RPCError(code: -32002, kind: "not_a_space", message: "\(path) is not a space") }
            return space
        }
        func info(_ space: Space) -> [String: Any] {
            ["path": path, "id": space.id, "name": space.name, "format": 0, "created_at": "2026-10-20T09:30:00.000Z", "nodes": space.nodes.count]
        }
        switch method {
        case "subscribe":
            clients[client] = true
            return (true, nil)
        case "service.status":
            return (["version": "0.0.0", "pid": 1, "spaces": spaces.keys.sorted(), "clients": clients.count], nil)
        case "space.create":
            guard spaces[path] == nil else { throw RPCError(code: -32003, kind: "already_exists", message: "\(path) already exists") }
            spaces[path] = Space(id: nextID(), name: params["name"] as? String ?? "")
            return (info(try space()), nil)
        case "space.open":
            if spaces[path] == nil, path.hasSuffix(".knode") {
                // A package on disk keeps the id of its manifest, as the real service reads it.
                let manifest = (try? Data(contentsOf: URL(filePath: path).appending(path: "manifest.json")))
                    .flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
                spaces[path] = Space(
                    id: manifest?["id"] as? String ?? nextID(),
                    name: manifest?["name"] as? String ?? URL(filePath: path).deletingPathExtension().lastPathComponent
                )
            }
            return (info(try space()), nil)
        case "node.create":
            let id = createNode(in: path, title: params["title"] as? String ?? "", body: params["body"] as? String ?? "", by: params["by"] as? [String: Any] ?? [:])
            return (spaces[path]!.nodes[id]!.json, (path, id, "create"))
        case "node.get":
            return (try space().nodes[params["id"] as? String ?? ""]?.json ?? NSNull(), nil)
        case "node.update":
            let id = params["id"] as? String ?? ""
            guard var node = try space().nodes[id] else { throw RPCError(code: -32001, kind: "not_found", message: "node \(id) not found") }
            clock += 1
            if let title = params["title"] as? String { node.title = title }
            if let body = params["body"] as? String { node.body = body }
            node.updatedAt = clock
            spaces[path]?.nodes[id] = node
            return (node.json, (path, id, "update"))
        case "history":
            let id = params["id"] as? String ?? ""
            guard let node = try space().nodes[id] else { return ([Any](), nil) }
            // One version per node: its creation, then its current text when it changed.
            var versions: [[String: Any]] = [[
                "seq": 1, "node_id": id, "op": "create", "changed_at": node.createdAt, "changed_by": node.by,
                "kind": "note", "title": node.title, "body": node.body, "tags": [String](), "change_id": 1,
            ]]
            if node.updatedAt != node.createdAt {
                versions.append([
                    "seq": 2, "node_id": id, "op": "update", "changed_at": node.updatedAt,
                    "changed_by": ["kind": "agent", "actor": "claude-code", "session": "5f1c2a"],
                    "kind": "note", "title": node.title, "body": node.body, "tags": [String](), "change_id": 2,
                ])
            }
            return (versions, nil)
        case "changes":
            _ = try space()
            return (changeLog[params["subject"] as? String ?? ""] ?? [], nil)
        case "undo":
            _ = try space()
            let change = (params["change"] as? NSNumber)?.int64Value ?? 0
            let force = params["force"] as? Bool ?? false
            if let later = conflicts[change], !force {
                throw RPCError(code: -32022, kind: "undo_conflict", message: "undoing change \(change) would overwrite later changes", data: ["later": later])
            }
            clock += 1
            let by = params["by"] as? [String: Any] ?? [:]
            return (["id": 900 + change, "changed_at": clock, "changed_by": by, "action": "undo", "subject": "", "undoes": change, "detail": [String: Any]()], nil)
        case "restore":
            let id = params["id"] as? String ?? ""
            guard var node = try space().nodes[id] else { throw RPCError(code: -32001, kind: "not_found", message: "node \(id) not found") }
            clock += 1
            node.body = "Restored \((params["version"] as? NSNumber)?.int64Value ?? 0)"
            node.updatedAt = clock
            spaces[path]?.nodes[id] = node
            return (node.json, (path, id, "update"))
        case "agents.list":
            return (agents, nil)
        case "grants.set":
            let name = params["agent"] as? String ?? ""
            let level = params["level"] as? String ?? ""
            guard let index = agents.firstIndex(where: { $0["name"] as? String == name }) else {
                throw RPCError(code: -32004, kind: "invalid", message: "no agent named \(name)")
            }
            var grants = (agents[index]["spaces"] as? [[String: Any]] ?? []).filter { $0["space"] as? String != path }
            grants.append(["space": path, "level": level])
            agents[index]["spaces"] = grants
            return (["space": path, "level": level], nil)
        case "agents.revoke":
            let name = params["agent"] as? String ?? ""
            guard let index = agents.firstIndex(where: { $0["name"] as? String == name }) else {
                throw RPCError(code: -32004, kind: "invalid", message: "no agent named \(name)")
            }
            clock += 1
            let keys = (agents[index]["keys"] as? [[String: Any]] ?? []).map { $0.merging(["revoked_at": clock]) { $1 } }
            let grants = (agents[index]["spaces"] as? [[String: Any]] ?? []).map { $0.merging(["level": "none"]) { $1 } }
            agents[index]["keys"] = keys
            agents[index]["spaces"] = grants
            return (agents[index], nil)
        case "recent":
            let nodes = try space().nodes.values.sorted { $0.updatedAt > $1.updatedAt }
            return (nodes.map { ["id": $0.id, "kind": "note", "title": $0.title, "created_at": $0.createdAt, "updated_at": $0.updatedAt, "updated_by": $0.by] }, nil)
        case "search":
            let query = (params["query"] as? String ?? "").lowercased()
            let hits = try space().nodes.values.filter { $0.title.lowercased().contains(query) || $0.body.lowercased().contains(query) }
            return (hits.map { ["node": ["id": $0.id, "kind": "note", "title": $0.title, "created_at": $0.createdAt, "updated_at": $0.updatedAt, "updated_by": $0.by], "snippet": "**\($0.title)**", "score": 1.0] }, nil)
        default:
            throw RPCError(code: -32601, kind: "", message: "no method \(method)")
        }
    }

    private func createNode(in path: String, title: String, body: String, by: [String: Any]) -> String {
        clock += 1
        let id = nextID()
        spaces[path]?.nodes[id] = StoredNode(id: id, title: title, body: body, createdAt: clock, updatedAt: clock, by: by)
        return id
    }

    private func nextID() -> String {
        counter += 1
        return String(format: "01K83N6V4W5D2XQJ0Y7RZ9%04d", counter)
    }

    private func notify(space: String, node: String, op: String) {
        let subscribers = lock.withLock { clients.filter(\.value).map(\.key) }
        for client in subscribers {
            send(["jsonrpc": "2.0", "method": "changed", "params": ["space": space, "node_id": node, "op": op]], to: client)
        }
    }

    private func send(_ message: [String: Any], to client: Int32) {
        guard var data = try? JSONSerialization.data(withJSONObject: message) else { return }
        data.append(0x0A)
        _ = data.withUnsafeBytes { write(client, $0.baseAddress, $0.count) }
    }
}
