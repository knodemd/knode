import Darwin
import Foundation
import Synchronization
@testable import Knode

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
        var settings: [String: String] = [:]
        var folders: [[String: Any]] = []
        /// The notes marked `markdown.deleted`, with the path their file had.
        var deletedFiles: [String: String] = [:]
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
    /// The documents of the bodies edited together (`doc.open`, `doc.edit`), by node id.
    private var documents: [String: FakeDocument] = [:]
    private var versionCounter = 0
    /// A body written through its document, announced after the answer.
    private var bodyNotice: [String: Any]?
    /// `space` notifications to send once the answer is sent.
    private var spaceEvents: [[String: Any]] = []
    /// The mode a `search` with `mode` runs in, whatever it asks for, and its note; `nil` runs
    /// the mode asked for.
    private var searchFallback: (mode: String, note: String?)?
    /// What `index.status` and `index.run` answer, and `index` in `service.status`.
    private var index: [String: Any] = FakeKnodeService.indexStatus()

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

    /// Marks a note as the live Markdown folder does when its file is deleted.
    func folderDeletesFile(of id: String, in path: String, file: String) {
        lock.withLock { spaces[path]?.deletedFiles[id] = file }
        notify(space: path, node: id, op: "update")
    }

    /// The notes marked `markdown.deleted`.
    func deletedFiles(in path: String) -> [String: String] {
        lock.withLock { spaces[path]?.deletedFiles ?? [:] }
    }

    /// Renames a space as another client would, and tells the subscribers.
    func otherClientRenames(_ path: String, to name: String) {
        lock.withLock { spaces[path]?.name = name }
        notifySpace(["space": path, "what": "renamed", "name": name])
    }

    /// Changes a setting as another client would, and tells the subscribers.
    func otherClientSets(_ key: String, to value: String?, in path: String) {
        lock.withLock { spaces[path]?.settings[key] = value }
        notifySpace(["space": path, "what": "settings", "key": key])
    }

    /// Adds a project folder as another client would, such as `knode folders add`.
    func otherClientAddsFolder(_ folderPath: String, in path: String) {
        let id = lock.withLock {
            let id = nextID()
            spaces[path]?.folders.append(folder(id: id, kind: "project", path: folderPath, capture: true, rules: [:]))
            return id
        }
        notifySpace(["space": path, "what": "folders", "folder": id])
    }

    func settings(of path: String) -> [String: String] {
        lock.withLock { spaces[path]?.settings ?? [:] }
    }

    func folders(of path: String) -> [[String: Any]] {
        lock.withLock { spaces[path]?.folders ?? [] }
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

    /// Makes `search` with a `mode` run in `mode` and answer with `note`, as when meaning
    /// cannot be used yet.
    func setSearchFallback(mode: String, note: String?) {
        lock.withLock { searchFallback = (mode, note) }
    }

    /// Sets what `index.status` answers.
    func setIndex(_ status: [String: Any]) {
        lock.withLock { index = status }
    }

    /// An `index.status` answer.
    static func indexStatus(
        enabled: Bool = true,
        state: String = "ready",
        downloaded: Int64 = 0,
        spaces: [[String: Any]] = [],
        paused: String? = nil
    ) -> [String: Any] {
        var status: [String: Any] = [
            "available": true, "enabled": enabled, "running": true,
            "model": ["name": "multilingual-e5-small", "state": state, "downloaded": downloaded, "size": 487_724_985],
            "spaces": spaces,
        ]
        if let paused { status["paused"] = paused }
        return status
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
        if let notice = lock.withLock({ () -> [String: Any]? in defer { bodyNotice = nil }; return bodyNotice }) { notifyChanged(notice) }
        let events = lock.withLock {
            defer { spaceEvents = [] }
            return spaceEvents
        }
        for event in events { notifySpace(event) }
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
            return (["version": "0.0.0", "pid": 1, "spaces": spaces.keys.sorted(), "clients": clients.count, "index": index], nil)
        case "index.status", "index.run":
            return (index, nil)
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
        case "space.rename":
            _ = try space()
            let name = (params["name"] as? String ?? "").trimmingCharacters(in: .whitespaces)
            spaces[path]?.name = name
            spaceEvents.append(["space": path, "what": "renamed", "name": name])
            return (info(try space()), nil)
        case "settings.get":
            return (try space().settings.merging(["space.id": try space().id]) { $1 }, nil)
        case "settings.set":
            _ = try space()
            let key = params["key"] as? String ?? ""
            var value = params["value"] as? String
            switch key {
            case "space.icon": break
            case "capture.enabled":
                guard value == nil || value == "on" || value == "off" else {
                    throw RPCError(code: -32004, kind: "invalid", message: "capture.enabled is on or off")
                }
                if value == "on" { value = nil }
            default: throw RPCError(code: -32004, kind: "invalid", message: "\(key) is not a setting Knode knows")
            }
            spaces[path]?.settings[key] = value
            spaceEvents.append(["space": path, "what": "settings", "key": key])
            return (try space().settings.merging(["space.id": try space().id]) { $1 }, nil)
        case "folders.list":
            return (try space().folders, nil)
        case "folders.set":
            let kind = params["kind"] as? String ?? ""
            guard ["project", "notes", "agent"].contains(kind) else {
                throw RPCError(code: -32004, kind: "invalid", message: "a folder is a project, notes or agent folder")
            }
            let rules = params["rules"] as? [String: Any] ?? [:]
            if kind == "agent", rules["agent"] == nil {
                throw RPCError(code: -32004, kind: "invalid", message: "an agent folder needs the rule agent")
            }
            let folderPath = params["path"] as? String ?? ""
            let folders = try space().folders
            let index = folders.firstIndex { $0["id"] as? String == params["id"] as? String || ($0["kind"] as? String == kind && $0["path"] as? String == folderPath) }
            let id = index.flatMap { folders[$0]["id"] as? String } ?? nextID()
            let saved = folder(id: id, kind: kind, path: folderPath, capture: params["capture"] as? Bool ?? true, rules: rules)
            if let index { spaces[path]?.folders[index] = saved } else { spaces[path]?.folders.append(saved) }
            spaceEvents.append(["space": path, "what": "folders", "folder": id])
            return (saved, nil)
        case "folders.remove":
            let id = params["id"] as? String ?? ""
            let before = try space().folders.count
            spaces[path]?.folders.removeAll { $0["id"] as? String == id }
            let removed = spaces[path]?.folders.count != before
            if removed { spaceEvents.append(["space": path, "what": "folders", "folder": id]) }
            return (removed, nil)
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
        case "node.delete":
            let id = params["id"] as? String ?? ""
            guard try space().nodes[id] != nil else { throw RPCError(code: -32001, kind: "not_found", message: "node \(id) not found") }
            spaces[path]?.nodes[id] = nil
            spaces[path]?.deletedFiles[id] = nil
            return (NSNull(), (path, id, "delete"))
        case "markdown.deleted.list":
            let space = try space()
            let marked = space.deletedFiles.compactMap { id, file in
                space.nodes[id].map { ["id": id, "title": $0.title, "path": file] as [String: Any] }
            }
            return (marked.sorted { ($0["path"] as? String ?? "") < ($1["path"] as? String ?? "") }, nil)
        case "markdown.deleted.keep":
            let id = params["id"] as? String ?? ""
            let kept = try space().deletedFiles[id] != nil
            spaces[path]?.deletedFiles[id] = nil
            return (kept, kept ? (path, id, "update") : nil)
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
            let found = try space().nodes.values
                .filter { $0.title.lowercased().contains(query) || $0.body.lowercased().contains(query) }
                .sorted { $0.title < $1.title }
            guard let asked = params["mode"] as? String else {
                return (found.map { ["node": ["id": $0.id, "kind": "note", "title": $0.title, "created_at": $0.createdAt, "updated_at": $0.updatedAt, "updated_by": $0.by], "snippet": "**\($0.title)**", "score": 1.0] }, nil)
            }
            let mode = searchFallback?.mode ?? asked
            // Words rank the notes found in order; meaning, here, ranks them the other way round.
            let hits: [[String: Any]] = found.enumerated().map { index, node in
                [
                    "node": ["id": node.id, "kind": "note", "title": node.title, "created_at": node.createdAt, "updated_at": node.updatedAt, "updated_by": node.by],
                    "snippet": "about **\(node.title)**", "score": 1.0,
                    "word_rank": mode == "meaning" ? NSNull() : index + 1,
                    "meaning_rank": mode == "words" ? NSNull() : found.count - index,
                ]
            }
            var result: [String: Any] = ["mode": mode, "hits": hits]
            if let note = searchFallback?.note { result["note"] = note }
            return (result, nil)
        case "doc.open", "doc.edit":
            return (try respondDocument(method, params, path: path), nil)
        default:
            throw RPCError(code: -32601, kind: "", message: "no method \(method)")
        }
    }

    /// A folder as `folders.list` answers. The fake records paths as given.
    private func folder(id: String, kind: String, path: String, capture: Bool, rules: [String: Any]) -> [String: Any] {
        clock += 1
        let by: [String: Any] = ["kind": "person", "actor": "Ada"]
        return [
            "id": id, "kind": kind, "path": path, "capture": capture, "rules": rules,
            "created_at": clock, "created_by": by, "updated_at": clock, "updated_by": by,
            "local": path, "missing": !FileManager.default.fileExists(atPath: path),
        ]
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

    private func notifySpace(_ params: [String: Any]) {
        let subscribers = lock.withLock { clients.filter(\.value).map(\.key) }
        for client in subscribers {
            send(["jsonrpc": "2.0", "method": "space", "params": params], to: client)
        }
    }

    private func send(_ message: [String: Any], to client: Int32) {
        guard var data = try? JSONSerialization.data(withJSONObject: message) else { return }
        data.append(0x0A)
        _ = data.withUnsafeBytes { write(client, $0.baseAddress, $0.count) }
    }
}

// MARK: - Editing together

/// A body's document in the fake: each version is a text, and the newest is the body.
struct FakeDocument {
    var texts: [String: String] = [:]
    var current = ""
}

extension FakeKnodeService {
    /// Rewrites a body as an agent would through Knode Service: the change is announced with the
    /// new version and the agent.
    func agentEditsBody(_ id: String, in path: String, body: String) {
        let notice: [String: Any] = lock.withLock {
            clock += 1
            spaces[path]?.nodes[id]?.body = body
            spaces[path]?.nodes[id]?.updatedAt = clock
            let version = mint(body, for: id)
            return ["space": path, "node_id": id, "op": "update", "version": version, "by": ["kind": "agent", "actor": "claude-code"]]
        }
        notifyChanged(notice)
    }

    /// Adds a node the service finds, as one read from a space on disk.
    func storeNode(_ id: String, title: String, body: String, in path: String) {
        lock.withLock {
            spaces[path]?.nodes[id] = StoredNode(id: id, title: title, body: body, createdAt: clock, updatedAt: clock, by: ["kind": "person", "actor": "Ada"])
        }
    }

    /// The text of a version of a body's document.
    func text(of version: String, node id: String) -> String? {
        lock.withLock { documents[id]?.texts[version] }
    }

    /// Answers `doc.open` and `doc.edit`; the caller holds the lock. Merges like the service: an
    /// edit made on an older text keeps what was written since.
    fileprivate func respondDocument(_ method: String, _ params: [String: Any], path: String) throws -> [String: Any] {
        let id = params["id"] as? String ?? ""
        guard let node = spaces[path]?.nodes[id] else { throw RPCError(code: -32001, kind: "not_found", message: "node \(id) not found") }
        if documents[id].map({ $0.texts[$0.current] != node.body }) ?? true {
            // A body written without the document: the document follows it.
            mint(node.body, for: id)
        }
        let document = documents[id]!
        if method == "doc.open" {
            return ["text": node.body, "version": document.current]
        }
        let version = params["version"] as? String ?? document.current
        guard let baseText = document.texts[version] else {
            throw RPCError(code: -32030, kind: "unknown_version", message: "the document has no version \(version)")
        }
        let splices = (params["splices"] as? [[String: Any]] ?? []).map {
            TextSplice(at: ($0["at"] as? NSNumber)?.intValue ?? 0, delete: ($0["delete"] as? NSNumber)?.intValue ?? 0, insert: $0["insert"] as? String ?? "")
        }
        let edited = TextDiff.apply(splices, to: baseText)
        let by = params["by"] as? [String: Any] ?? [:]
        if version == document.current {
            guard edited != node.body else { return ["version": version, "seen": version] }
            let new = writeBody(edited, to: id, in: path, by: by)
            return ["version": new, "seen": new]
        }
        let seen = mint(edited, for: id, current: false)
        // The edit, moved through what others wrote since its base.
        let theirs = TextDiff.patches(from: baseText, to: node.body)
        let mine = TextDiff.patches(from: baseText, to: edited).map { patch in
            TextPatch(range: TextDiff.transform(patch.range, through: theirs), replacement: patch.replacement)
        }
        let merged = TextDiff.apply(mine, to: node.body)
        let new = merged == node.body ? document.current : writeBody(merged, to: id, in: path, by: by)
        return ["version": new, "seen": seen, "text": merged]
    }

    private func writeBody(_ body: String, to id: String, in path: String, by: [String: Any]) -> String {
        clock += 1
        spaces[path]?.nodes[id]?.body = body
        spaces[path]?.nodes[id]?.updatedAt = clock
        let version = mint(body, for: id)
        bodyNotice = ["space": path, "node_id": id, "op": "update", "version": version, "by": by]
        return version
    }

    @discardableResult
    private func mint(_ text: String, for id: String, current: Bool = true) -> String {
        versionCounter += 1
        let version = "{\"\(versionCounter)\":1}"
        documents[id, default: FakeDocument()].texts[version] = text
        if current { documents[id]?.current = version }
        return version
    }

    private func notifyChanged(_ params: [String: Any]) {
        let subscribers = lock.withLock { clients.filter(\.value).map(\.key) }
        for client in subscribers {
            send(["jsonrpc": "2.0", "method": "changed", "params": params], to: client)
        }
    }
}

