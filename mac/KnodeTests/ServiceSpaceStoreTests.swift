import Foundation
import Testing
@testable import Knode

/// Waits until `condition` holds, for at most two seconds.
func eventually(isolation: isolated (any Actor)? = #isolation, _ condition: () async throws -> Bool) async throws -> Bool {
    for _ in 0..<200 {
        if try await condition() { return true }
        try await Task.sleep(for: .milliseconds(10))
    }
    return false
}

/// `ServiceConnection`, `ServiceSpaceStore` and `RoutingSpaceStore` against a fake Knode Service.
@Suite(.serialized) final class ServiceSpaceStoreTests {
    let service: FakeKnodeService
    let path = "/tmp/Test.knode"

    init() throws {
        service = try FakeKnodeService()
    }

    deinit {
        service.stop()
    }

    var url: URL { URL(filePath: path, directoryHint: .isDirectory) }

    // MARK: Connection

    @Test func connectionCallsAndDecodes() async throws {
        let connection = try ServiceConnection.connect(to: service.socketURL)
        defer { connection.close() }
        let status = try await connection.call("service.status", as: ServiceMonitor.Status.self)
        #expect(status.version == "0.0.0")
        #expect(status.clients == 1)
        _ = try await connection.call("space.open", ["space": .string(path)], as: ServiceSpaceInfo.self)
        let missing = try await connection.call("node.get", ["space": .string(path), "id": "x"], as: WireNode?.self)
        #expect(missing == nil)
    }

    @Test func connectionAuthenticatesAsTheOwner() async throws {
        // Without an owner token next to the socket, nothing is sent first.
        let plain = try ServiceConnection.connect(to: service.socketURL)
        _ = try await plain.call("service.status", as: ServiceMonitor.Status.self)
        plain.close()
        #expect(service.requests.first == "service.status")

        let token = service.socketURL.deletingLastPathComponent().appending(path: "owner.token")
        try "5f0c\n".write(to: token, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: token) }
        let connection = try ServiceConnection.connect(to: service.socketURL)
        defer { connection.close() }
        _ = try await connection.call("service.status", as: ServiceMonitor.Status.self)
        #expect(Array(service.requests.suffix(2)) == ["auth", "service.status"])
    }

    @Test func connectionReportsRemoteErrors() async throws {
        let connection = try ServiceConnection.connect(to: service.socketURL)
        defer { connection.close() }
        await #expect(throws: ServiceError.remote(code: -32601, kind: "", message: "no method nope")) {
            _ = try await connection.call("nope", as: Bool.self)
        }
    }

    @Test func connectingWithoutAServiceFails() {
        #expect(throws: ServiceError.self) {
            _ = try ServiceConnection.connect(to: URL(filePath: "/tmp/kn-none/service.sock"))
        }
    }

    @Test func requestsFailWhenTheServiceStops() async throws {
        let connection = try ServiceConnection.connect(to: service.socketURL)
        _ = try await connection.call("subscribe", as: Bool.self)
        service.stop()
        #expect(try await eventually { connection.isClosed })
        await #expect(throws: ServiceError.disconnected) {
            _ = try await connection.call("service.status", as: ServiceMonitor.Status.self)
        }
    }

    // MARK: Store

    @Test func storeWritesAndReadsThroughTheService() async throws {
        let store = ServiceSpaceStore(socketURL: service.socketURL, person: "Ada")
        #expect(await store.isAvailable())
        let space = try await store.openSpace(at: url)
        #expect(space.name == "Test")

        let node = try await store.createNode(in: space.id, title: "Plan", body: "# Ship\n\nShip v0.1.")
        let stored = try #require(service.node(node.id, in: path))
        #expect(stored.by["actor"] as? String == "Ada")
        #expect(stored.by["kind"] as? String == "person")

        let renamed = try await store.updateNode(node.id, in: space.id, title: "  ", body: "Ship it.")
        #expect(renamed.title == "Plan")
        #expect(renamed.body == "Ship it.")

        let list = try await store.listNodes(in: space.id)
        #expect(list.map(\.title) == ["Plan"])
        #expect(list.first?.excerpt == "Ship it.")

        let hits = try await store.search("plan", in: space.id)
        #expect(hits.first?.excerpt == "Plan")

        await #expect(throws: SpaceStoreError.nodeNotFound("01NOPE")) {
            _ = try await store.node("01NOPE", in: space.id)
        }
        await #expect(throws: SpaceStoreError.alreadyExists(url)) {
            _ = try await store.createSpace(at: url, name: "Test")
        }
    }

    @Test func storeSeesChangesMadeByOthers() async throws {
        let store = ServiceSpaceStore(socketURL: service.socketURL, person: "Ada")
        let space = try await store.openSpace(at: url)
        let changes = store.changes()
        let id = service.agentCreatesNode(in: path, title: "From Claude", body: "Remembered.")

        var iterator = changes.makeAsyncIterator()
        let change = await iterator.next()
        #expect(change == SpaceChange(space: space.id, nodeID: id, kind: .create))
        #expect(try await store.listNodes(in: space.id).first?.excerpt == "Remembered.")
    }

    @Test func storeIsUnavailableWithoutAService() async {
        let store = ServiceSpaceStore(socketURL: URL(filePath: "/tmp/kn-none/service.sock"))
        #expect(await store.isAvailable() == false)
        await #expect(throws: ServiceError.self) {
            _ = try await store.openSpace(at: self.url)
        }
    }

    // MARK: Routing

    @Test func routingUsesTheServiceWhenItRuns() async throws {
        let core = CoreSpaceStore(person: "Ada")
        let store = RoutingSpaceStore(service: ServiceSpaceStore(socketURL: service.socketURL, person: "Ada"), core: core)
        let space = try await store.openSpace(at: url)
        #expect(await store.backend(of: space.id) == .service)
        _ = try await store.createNode(in: space.id, title: "Plan", body: "")
        #expect(service.requests.contains("node.create"))
    }

    @Test func routingFallsBackToTheCoreAndMovesBack() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "KnodeTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let spaceURL = folder.appending(path: "Real.knode", directoryHint: .isDirectory)

        // No service: the core in the app holds the space.
        let socket = service.socketURL
        service.stop()
        let core = CoreSpaceStore(person: "Ada")
        let store = RoutingSpaceStore(service: ServiceSpaceStore(socketURL: socket, person: "Ada"), core: core)
        let space = try await store.createSpace(at: spaceURL, name: "Real")
        #expect(await store.backend(of: space.id) == .core)
        _ = try await store.createNode(in: space.id, title: "Offline", body: "Written without the service.")
        #expect(try await store.listNodes(in: space.id).map(\.title) == ["Offline"])
    }

    @Test func routingMovesToTheCoreWhenTheServiceStops() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "KnodeTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let spaceURL = folder.appending(path: "Real.knode", directoryHint: .isDirectory)
        // The space exists on disk, as the real service would have made it.
        let core = CoreSpaceStore(person: "Ada")
        let made = try await core.createSpace(at: spaceURL, name: "Real")
        core.close(made.id)

        let store = RoutingSpaceStore(service: ServiceSpaceStore(socketURL: service.socketURL, person: "Ada"), core: core)
        let changes = store.changes()
        let space = try await store.openSpace(at: spaceURL)
        #expect(await store.backend(of: space.id) == .service)

        service.stop()
        // The list now comes from the core, and the app is told to reload.
        #expect(try await store.listNodes(in: space.id).isEmpty)
        #expect(await store.backend(of: space.id) == .core)
        var iterator = changes.makeAsyncIterator()
        #expect(await iterator.next()?.kind == .reload)
    }

    @Test func historyComesFromTheService() async throws {
        let store = ServiceSpaceStore(socketURL: service.socketURL, person: "Ada")
        let space = try await store.createSpace(at: url, name: "Test")
        let node = try await store.createNode(in: space.id, title: "Plan", body: "One")
        _ = try await store.updateNode(node.id, in: space.id, title: nil, body: "Two")
        let history = try await store.history(of: node.id, in: space.id)
        #expect(history.map(\.operation) == [.update, .create])
        #expect(history.first?.author == Author(kind: .agent, name: "claude-code", session: "5f1c2a"))
        #expect(history.last?.author == Author(kind: .person, name: "Ada"))
        #expect(history.first?.changeID == 2)
        #expect(service.requests.contains("history"))
    }

    @Test func routingRenamesWithTheCoreWhileTheServiceHoldsTheSpace() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "KnodeTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let spaceURL = folder.appending(path: "Real.knode", directoryHint: .isDirectory)
        // A real package, as the service would have written it.
        let core = CoreSpaceStore(person: "Ada")
        let made = try await core.createSpace(at: spaceURL, name: "Real")
        core.close(made.id)

        let store = RoutingSpaceStore(service: ServiceSpaceStore(socketURL: service.socketURL, person: "Ada"), core: core)
        let space = try await store.openSpace(at: spaceURL)
        #expect(await store.backend(of: space.id) == .service)

        let renamed = try await store.renameSpace(space.id, to: "Renamed")
        #expect(renamed.name == "Renamed")
        try await store.setIcon("🧪", of: space.id)
        #expect(try await store.icon(of: space.id) == "🧪")
        // The service still holds the space, and the core let go of it.
        #expect(await store.backend(of: space.id) == .service)
        #expect(!core.isOpen(space.id))
        #expect(!service.requests.contains("node.update"))

        // Opened again through the service, it shows the new name and icon.
        await store.closeSpace(space.id)
        let again = try await store.openSpace(at: spaceURL)
        #expect(again.name == "Renamed")
        #expect(again.icon == "🧪")
    }

    // MARK: History through the service

    @Test func storeReadsTheChangesOfANote() async throws {
        let store = ServiceSpaceStore(socketURL: service.socketURL, person: "Ada")
        let space = try await store.createSpace(at: url, name: "Test")
        let node = try await store.createNode(in: space.id, title: "Plan", body: "One")
        service.setChanges([
            ["id": 3, "changed_at": 1_792_489_800_300, "changed_by": ["kind": "person", "actor": "Ada"], "action": "undo", "subject": node.id, "undoes": 2, "detail": [String: Any]()],
            ["id": 2, "changed_at": 1_792_489_800_200, "changed_by": ["kind": "agent", "actor": "claude-code", "session": "5f1c"], "action": "node.update", "subject": node.id, "detail": [String: Any]()],
        ], for: node.id)

        let changes = try await store.changes(of: node.id, in: space.id, limit: 50)
        #expect(changes.map(\.id) == [3, 2])
        #expect(changes[0].undoes == 2)
        #expect(changes[0].author == Author(kind: .person, name: "Ada"))
        #expect(changes[1].author == Author(kind: .agent, name: "claude-code", session: "5f1c"))
        #expect(changes[1].action == ChangeRecord.Action.update)
        #expect(service.params(of: "changes")?["subject"] as? String == node.id)
        #expect(service.params(of: "changes")?["limit"] as? Int == 50)
    }

    @Test func storeUndoesThroughTheServiceAndReportsConflicts() async throws {
        let store = ServiceSpaceStore(socketURL: service.socketURL, person: "Ada")
        let space = try await store.createSpace(at: url, name: "Test")
        service.setConflict(undoing: 7, later: [8, 9])

        await #expect(throws: SpaceStoreError.undoConflict(change: 7, later: [8, 9], message: "undoing change 7 would overwrite later changes")) {
            try await store.undo(change: 7, in: space.id, force: false)
        }
        #expect(service.params(of: "undo")?["force"] as? Bool == false)

        try await store.undo(change: 7, in: space.id, force: true)
        let params = try #require(service.params(of: "undo"))
        #expect(params["force"] as? Bool == true)
        #expect(params["change"] as? Int == 7)
        #expect((params["by"] as? [String: Any])?["actor"] as? String == "Ada")
        #expect((params["by"] as? [String: Any])?["kind"] as? String == "person")
    }

    @Test func storeRestoresThroughTheService() async throws {
        let store = ServiceSpaceStore(socketURL: service.socketURL, person: "Ada")
        let space = try await store.createSpace(at: url, name: "Test")
        let node = try await store.createNode(in: space.id, title: "Plan", body: "One")
        let restored = try await store.restore(node.id, to: 1, in: space.id)
        #expect(restored.body == "Restored 1")
        #expect(service.params(of: "restore")?["version"] as? Int == 1)
        await #expect(throws: SpaceStoreError.nodeNotFound("01NOPE")) {
            _ = try await store.restore("01NOPE", to: 1, in: space.id)
        }
    }

    @Test func routingUndoesAndRestoresThroughTheServiceWhenItRuns() async throws {
        let core = CoreSpaceStore(person: "Ada")
        let store = RoutingSpaceStore(service: ServiceSpaceStore(socketURL: service.socketURL, person: "Ada"), core: core)
        #expect(store.person == "Ada")
        let space = try await store.openSpace(at: url)
        let node = try await store.createNode(in: space.id, title: "Plan", body: "One")
        _ = try await store.restore(node.id, to: 1, in: space.id)
        try await store.undo(change: 1, in: space.id, force: false)
        _ = try await store.changes(of: node.id, in: space.id, limit: 10)
        #expect(service.requests.contains("restore"))
        #expect(service.requests.contains("undo"))
        #expect(service.requests.contains("changes"))
        // The core in the app never opened the space.
        #expect(!core.isOpen(space.id))
    }

    @Test func routingUndoesWithTheCoreWithoutAService() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "KnodeTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let socket = service.socketURL
        service.stop()
        let store = RoutingSpaceStore(service: ServiceSpaceStore(socketURL: socket, person: "Ada"), core: CoreSpaceStore(person: "Ada"))
        let space = try await store.createSpace(at: folder.appending(path: "Real.knode", directoryHint: .isDirectory), name: "Real")
        let node = try await store.createNode(in: space.id, title: "Plan", body: "One")
        _ = try await store.updateNode(node.id, in: space.id, title: nil, body: "Two")

        let changes = try await store.changes(of: node.id, in: space.id, limit: 10)
        #expect(changes.map(\.action) == [ChangeRecord.Action.update, ChangeRecord.Action.create])
        try await store.undo(change: changes[0].id, in: space.id, force: false)
        #expect(try await store.node(node.id, in: space.id).body == "One")
        let restored = try await store.restore(node.id, to: try #require(try await store.history(of: node.id, in: space.id).first(where: { $0.body == "Two" })).seq, in: space.id)
        #expect(restored.body == "Two")
    }

    // MARK: App

    @MainActor
    @Test func theListFollowsAnAgentsWrites() async throws {
        let store = ServiceSpaceStore(socketURL: service.socketURL, person: "Ada")
        let model = AppModel(store: store, searchDelay: .zero, saveDelay: .seconds(60))
        await model.openSpace(at: url)
        #expect(model.nodes.isEmpty)

        let id = service.agentCreatesNode(in: path, title: "From Claude", body: "First.")
        #expect(try await eventually { model.nodes.map(\.id) == [id] })

        await model.openNode(id)
        #expect(model.editor?.body == "First.")
        service.agentUpdatesNode(id, in: path, body: "Second.")
        #expect(try await eventually { model.editor?.body == "Second." })
    }
}
