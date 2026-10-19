import Foundation
import Testing
@testable import Knode

@MainActor
@Suite struct AppModelTests {
    let url = URL(filePath: "/tmp/Test.knode")

    func makeModel() -> AppModel {
        let clock = TestClock()
        return AppModel(
            store: PreviewSpaceStore(now: { clock.now() }),
            searchDelay: .zero,
            saveDelay: .seconds(60)
        )
    }

    @Test func startsWithNothingOpen() {
        let model = makeModel()
        #expect(model.spaces.isEmpty)
        #expect(model.selectedSpace == nil)
        #expect(model.nodes.isEmpty)
        #expect(model.editor == nil)
    }

    @Test func openingASpaceSelectsItAndListsItsNodes() async {
        let model = makeModel()
        await model.openSpace(at: url)
        #expect(model.spaces.count == 1)
        #expect(model.selectedSpace?.name == "Test")
        #expect(model.nodes.count == 4)
    }

    @Test func openingAFileThatIsNotASpaceShowsAnError() async {
        let model = makeModel()
        await model.openSpace(at: URL(filePath: "/tmp/notes.txt"))
        #expect(model.spaces.isEmpty)
        #expect(model.errorMessage != nil)
    }

    @Test func openingANodeLoadsItIntoTheEditor() async throws {
        let model = makeModel()
        await model.openSpace(at: url)
        let first = try #require(model.nodes.first)
        await model.openNode(first.id)
        #expect(model.editor?.nodeID == first.id)
        #expect(model.editor?.title == first.title)
    }

    @Test func editingSavesAndMovesTheNoteToTheTop() async throws {
        let model = makeModel()
        await model.openSpace(at: url)
        let last = try #require(model.nodes.last)
        await model.openNode(last.id)
        let editor = try #require(model.editor)

        editor.title = "Renamed"
        editor.body = "# Changed\n\nNew text"
        #expect(editor.hasUnsavedChanges)
        await editor.save()

        #expect(!editor.hasUnsavedChanges)
        #expect(model.nodes.first?.id == last.id)
        #expect(model.nodes.first?.title == "Renamed")
        #expect(model.nodes.first?.excerpt == "Changed")
        #expect(model.nodes.count == 4)
    }

    @Test func switchingNotesSavesTheOpenOne() async throws {
        let model = makeModel()
        await model.openSpace(at: url)
        let (first, second) = (model.nodes[0], model.nodes[1])
        await model.openNode(first.id)
        model.editor?.body = "Edited before switching"
        await model.openNode(second.id)

        await model.openNode(first.id)
        #expect(model.editor?.body == "Edited before switching")
    }

    @Test func searchFiltersTheList() async {
        let model = makeModel()
        await model.openSpace(at: url)
        model.searchText = "links"
        await model.reload()
        #expect(model.nodes.map(\.title).first == "Example: How links work")
        #expect(model.nodes.count < 4)

        model.searchText = ""
        await model.reload()
        #expect(model.nodes.count == 4)
    }

    @Test func creatingANoteOpensIt() async {
        let model = makeModel()
        await model.openSpace(at: url)
        await model.createNode()
        #expect(model.nodes.count == 5)
        #expect(model.nodes.first?.title == "Untitled")
        #expect(model.editor?.nodeID == model.nodes.first?.id)
    }

    @Test func followsWikiLinksByTitle() async throws {
        let model = makeModel()
        await model.openSpace(at: url)
        await model.openLinkedNode(titled: "example: how links work")
        #expect(model.editor?.title == "Example: How links work")
    }

    @Test func newSpacesStartEmpty() async {
        let model = makeModel()
        await model.createSpace(at: URL(filePath: "/tmp/Work.knode"))
        #expect(model.selectedSpace?.name == "Work")
        #expect(model.nodes.isEmpty)
    }

    @Test func theExampleSpaceLivesInTheExampleStore() async throws {
        let clock = TestClock()
        // A main store with no examples, to tell the two apart.
        let model = AppModel(
            store: PreviewSpaceStore(seedExamples: false, now: { clock.now() }),
            exampleStore: PreviewSpaceStore(now: { clock.now() }),
            searchDelay: .zero,
            saveDelay: .seconds(60)
        )
        await model.openExampleSpace()
        #expect(model.isShowingExample)
        #expect(model.nodes.count == 4)
        let example = try #require(model.selectedSpaceID)

        await model.openSpace(at: url)
        #expect(!model.isShowingExample)
        #expect(model.nodes.isEmpty)

        await model.selectSpace(example)
        #expect(model.isShowingExample)
        await model.createNode()
        #expect(model.nodes.count == 5)
    }

    @Test func remembersRecentSpacesButNotTheExample() async throws {
        let suite = "KnodeTests-\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let folder = FileManager.default.temporaryDirectory.appending(path: suite, directoryHint: .isDirectory)
        defer { try? FileManager.default.removeItem(at: folder) }
        let spaceURL = folder.appending(path: "Work.knode", directoryHint: .isDirectory)

        let model = AppModel(
            store: CoreSpaceStore(person: "Ada"),
            recentSpaces: RecentSpaces(defaults: defaults),
            searchDelay: .zero,
            saveDelay: .seconds(60)
        )
        await model.createSpace(at: spaceURL)
        await model.openExampleSpace()
        #expect(model.recentSpaceURLs.map(\.lastPathComponent) == ["Work.knode"])

        // A new window of the app finds it again.
        let next = AppModel(store: CoreSpaceStore(person: "Ada"), recentSpaces: RecentSpaces(defaults: defaults))
        #expect(next.recentSpaceURLs == model.recentSpaceURLs)

        model.clearRecentSpaces()
        #expect(model.recentSpaceURLs.isEmpty)
    }
}
