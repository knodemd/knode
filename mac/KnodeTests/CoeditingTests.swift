import AppKit
import Foundation
import Testing
@testable import Knode

/// Differences between texts, and how a selection follows them.
struct TextDiffTests {
    @Test(arguments: [
        ("", "Hello"),
        ("Hello", ""),
        ("Hello world", "Hello brave world"),
        ("one two three", "zero one three four"),
        ("caf\u{E9} 😀 ok", "caf\u{E9} 😃 ok!"),
        ("# Plan\n\nShip v0.3.\n", "# Plan\n\nShip v0.3 on Friday.\n- tests\n"),
        ("👩‍👩‍👧 family", "👩‍👩‍👦 family"),
    ])
    func patchesAndSplicesTurnOneTextIntoTheOther(old: String, new: String) {
        #expect(TextDiff.apply(TextDiff.patches(from: old, to: new), to: old) == new)
        #expect(TextDiff.apply(TextDiff.splices(from: old, to: new), to: old) == new)
    }

    @Test func patchesTouchOnlyWhatChanged() {
        let patches = TextDiff.patches(from: "alpha beta gamma", to: "alpha BETA gamma!")
        #expect(patches.count == 2)
        #expect(patches.first?.range.location == 6)
        #expect(patches.last?.range.location == 16)
    }

    @Test func longTextsAreComparedByLine() {
        let line = String(repeating: "word ", count: 20) + "\n"
        let old = String(repeating: line, count: 400)
        var lines = old.components(separatedBy: "\n")
        lines[10] = "changed near the top"
        lines[390] += " and near the end"
        let new = lines.joined(separator: "\n")
        let patches = TextDiff.patches(from: old, to: new)
        #expect(patches.count >= 2)
        #expect(patches.allSatisfy { $0.range.length < 200 })
        #expect(TextDiff.apply(patches, to: old) == new)
    }

    @Test func spliceCountsUnicodeScalars() {
        // "😀" is two UTF-16 units and one scalar.
        let splice = TextDiff.splice(in: "😀 ab", replacing: NSRange(location: 3, length: 1), with: "x")
        #expect(splice == TextSplice(at: 2, delete: 1, insert: "x"))
        // Half of a surrogate pair is not a splice.
        #expect(TextDiff.splice(in: "😀", replacing: NSRange(location: 1, length: 1), with: "") == nil)
    }

    @Test func theCursorFollowsTheTextAroundIt() {
        let old = "Hello world"
        // Before the cursor: it moves right.
        let before = TextDiff.patches(from: old, to: "Oh, Hello world")
        #expect(TextDiff.transform(NSRange(location: 6, length: 0), through: before) == NSRange(location: 10, length: 0))
        // After the cursor: it stays.
        let after = TextDiff.patches(from: old, to: "Hello world!!")
        #expect(TextDiff.transform(NSRange(location: 6, length: 0), through: after) == NSRange(location: 6, length: 0))
        // Inserted right at the cursor: the cursor stays before it.
        let at = TextDiff.patches(from: old, to: "Hello big world")
        #expect(TextDiff.transform(NSRange(location: 6, length: 0), through: at) == NSRange(location: 6, length: 0))
        // Deleted around the cursor: it goes to where the text was.
        let deleted = TextDiff.patches(from: old, to: "Hello")
        #expect(TextDiff.transform(NSRange(location: 8, length: 0), through: deleted) == NSRange(location: 5, length: 0))
    }

    @Test func aSelectionKeepsCoveringItsWords() {
        let old = "one two three"
        let new = "zero one two three four"
        let patches = TextDiff.patches(from: old, to: new)
        // "two" stays selected.
        let selected = TextDiff.transform(NSRange(location: 4, length: 3), through: patches)
        #expect((new as NSString).substring(with: selected) == "two")
        // A selection whose words were replaced shrinks to what is left of it.
        let replaced = TextDiff.patches(from: old, to: "one 2 three")
        let shrunk = TextDiff.transform(NSRange(location: 4, length: 3), through: replaced)
        #expect(shrunk.location == 4)
        #expect(shrunk.length <= 1)
    }

    @Test @MainActor func theTextViewIsPatchedInPlaceAndKeepsTheSelection() throws {
        let node = Node(id: "n1", title: "Plan", body: "Hello world", createdAt: .now, updatedAt: .now)
        let editor = NoteEditorModel(node: node, spaceID: "s1", store: PreviewSpaceStore())
        let coordinator = MarkdownSourceEditor.Coordinator(editor: editor)
        let textView = NSTextView()
        textView.string = "Hello world"
        coordinator.textView = textView
        textView.setSelectedRange(NSRange(location: 6, length: 5))  // "world"
        coordinator.show("Oh, Hello world, again")
        #expect(textView.string == "Oh, Hello world, again")
        #expect((textView.string as NSString).substring(with: textView.selectedRange()) == "world")
    }
}

/// The note editor against a fake Knode Service: typing sent as splices, agent edits shown live.
@Suite(.serialized) @MainActor final class CoeditingEditorTests {
    let service: FakeKnodeService
    let path = "/tmp/Coedit.knode"

    init() throws {
        service = try FakeKnodeService()
    }

    deinit {
        service.stop()
    }

    private func open(_ body: String) async throws -> (ServiceSpaceStore, NoteEditorModel, Task<Void, Never>) {
        let store = ServiceSpaceStore(socketURL: service.socketURL, person: "Ada")
        let space = try await store.createSpace(at: URL(filePath: path, directoryHint: .isDirectory), name: "Coedit")
        let node = try await store.createNode(in: space.id, title: "Plan", body: body)
        let document = try await store.openBody(node.id, in: space.id)
        let editor = NoteEditorModel(node: node, spaceID: space.id, store: store, document: document, typingDelay: .milliseconds(20), presenceDuration: .seconds(30))
        // What AppModel does with each change.
        let changes = store.changes()
        let listening = Task { @MainActor [weak editor] in
            for await change in changes where change.nodeID == node.id {
                editor?.remoteChanged(version: change.version, author: change.author)
            }
        }
        return (store, editor, listening)
    }

    @Test func typingIsSentAsSplicesBasedOnTheVersion() async throws {
        let (_, editor, listening) = try await open("Hello")
        defer { listening.cancel() }
        #expect(editor.isShared)
        editor.viewDidEdit(text: "Hello!", range: NSRange(location: 5, length: 0), replacement: "!")
        editor.viewDidEdit(text: "Hello!!", range: NSRange(location: 6, length: 0), replacement: "!")
        #expect(editor.hasUnsavedChanges)
        #expect(try await eventually { !editor.hasUnsavedChanges })
        // Both keystrokes went in one write.
        #expect(service.requests.filter { $0 == "doc.edit" }.count == 1)
        let params = try #require(service.params(of: "doc.edit"))
        let splices = params["splices"] as? [[String: Any]]
        #expect(splices?.count == 2)
        #expect(splices?.first?["at"] as? Int == 5)
        #expect(params["version"] is String)
        let id = editor.nodeID
        #expect(service.node(id, in: path)?.body == "Hello!!")
    }

    @Test func anAgentsEditAppearsLiveWithWhoMadeIt() async throws {
        let (_, editor, listening) = try await open("Ship v0.3.")
        defer { listening.cancel() }
        service.agentEditsBody(editor.nodeID, in: path, body: "Ship v0.3 on Friday.")
        #expect(try await eventually { editor.body == "Ship v0.3 on Friday." })
        #expect(editor.otherWriter?.name == "claude-code")
        #expect(editor.otherWriter?.editorName == "Claude Code")
        #expect(!editor.hasUnsavedChanges)
    }

    @Test func whatThePersonTypesMergesWithTheAgentsEdit() async throws {
        let (_, editor, listening) = try await open("Ship v0.3.")
        defer { listening.cancel() }
        // The person types at the start while the agent rewrites the end.
        editor.viewDidEdit(text: "Soon: Ship v0.3.", range: NSRange(location: 0, length: 0), replacement: "Soon: ")
        service.agentEditsBody(editor.nodeID, in: path, body: "Ship v0.3 on Friday.")
        #expect(try await eventually { editor.body == "Soon: Ship v0.3 on Friday." })
        #expect(try await eventually { !editor.hasUnsavedChanges })
        #expect(service.node(editor.nodeID, in: path)?.body == "Soon: Ship v0.3 on Friday.")
        // Typing goes on from the merged text.
        editor.viewDidEdit(text: "Soon: Ship v0.3 on Friday!", range: NSRange(location: 25, length: 1), replacement: "!")
        #expect(try await eventually { service.node(editor.nodeID, in: path)?.body == "Soon: Ship v0.3 on Friday!" })
    }

    @Test func ownWritesShowNoPresence() async throws {
        let (_, editor, listening) = try await open("a")
        defer { listening.cancel() }
        editor.body = "ab"
        #expect(try await eventually { service.node(editor.nodeID, in: path)?.body == "ab" })
        try await Task.sleep(for: .milliseconds(100))
        #expect(editor.otherWriter == nil)
    }

    @Test func withoutTheServiceTheBodyIsSavedWhole() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "KnodeTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let spaceURL = folder.appending(path: "Real.knode", directoryHint: .isDirectory)
        let core = CoreSpaceStore(person: "Ada")
        let made = try await core.createSpace(at: spaceURL, name: "Real")
        let node = try await core.createNode(in: made.id, title: "Plan", body: "One")
        core.close(made.id)

        let store = RoutingSpaceStore(service: ServiceSpaceStore(socketURL: service.socketURL, person: "Ada"), core: core)
        let space = try await store.openSpace(at: spaceURL)
        service.storeNode(node.id, title: "Plan", body: "One", in: ServiceSpaceStore.path(of: spaceURL))
        let editor = await NoteEditorModel.open(node, spaceID: space.id, store: store)
        #expect(editor.isShared)

        service.stop()
        editor.body = "One two"
        await editor.save()
        #expect(!editor.isShared)
        #expect(!editor.hasUnsavedChanges)
        #expect(try await core.node(node.id, in: space.id).body == "One two")

        // A store that never edits together saves whole from the start.
        let alone = await NoteEditorModel.open(node, spaceID: space.id, store: PreviewSpaceStore())
        #expect(!alone.isShared)
    }
}
