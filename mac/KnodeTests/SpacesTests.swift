import Foundation
import Testing
@testable import Knode

/// Several spaces in the sidebar: create, switch, rename, icon, remove, the default space.
@MainActor
@Suite final class SpacesTests {
    let folder: URL
    let defaults: UserDefaults
    let suite = "KnodeTests-\(UUID().uuidString)"

    init() throws {
        folder = FileManager.default.temporaryDirectory.appending(path: "KnodeTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defaults = try #require(UserDefaults(suiteName: suite))
    }

    deinit {
        try? FileManager.default.removeItem(at: folder)
        UserDefaults().removePersistentDomain(forName: suite)
    }

    func makeModel(settings: MemoryKnodeSettings = MemoryKnodeSettings()) -> AppModel {
        let clock = TestClock()
        return AppModel(
            store: PreviewSpaceStore(seedExamples: false, now: { clock.now() }),
            library: SpaceLibrary(defaults: defaults),
            settings: settings,
            searchDelay: .zero,
            saveDelay: .seconds(60)
        )
    }

    @Test func createdSpacesAppearInTheSidebarInOrder() async {
        let model = makeModel()
        await model.createSpace(named: "Work", in: folder)
        await model.createSpace(named: "Personal", in: folder)
        #expect(model.sidebarSpaces.map(\.name) == ["Work", "Personal"])
        #expect(model.sidebarSpaces.first?.url == folder.appending(path: "Work.knode", directoryHint: .isDirectory).standardizedFileURL)
        #expect(model.selectedSpace?.name == "Personal")
        #expect(model.selectedSpaceURL == model.sidebarSpaces.last?.url)
    }

    @Test func aNameWithASlashMakesAValidFileName() async {
        let model = makeModel()
        await model.createSpace(named: "Work/Home", in: folder)
        #expect(model.selectedSpace?.name == "Work/Home")
        #expect(model.selectedSpace?.url?.lastPathComponent == "Work-Home.knode")
        await model.createSpace(named: "  ", in: folder)
        #expect(model.errorMessage != nil)
    }

    @Test func switchesSpacesByNumber() async {
        let model = makeModel()
        await model.createSpace(named: "Work", in: folder)
        await model.createSpace(named: "Personal", in: folder)
        await model.selectSpace(number: 1)
        #expect(model.selectedSpace?.name == "Work")
        await model.selectSpace(number: 2)
        #expect(model.selectedSpace?.name == "Personal")
        await model.selectSpace(number: 9)
        #expect(model.selectedSpace?.name == "Personal")
    }

    @Test func theListIsKeptBetweenLaunches() async {
        let model = makeModel()
        await model.createSpace(named: "Work", in: folder)
        let url = model.sidebarSpaces[0].url
        await model.setIcon("🌱", forSpaceAt: url)

        let again = makeModel()
        #expect(again.sidebarSpaces.map(\.name) == ["Work"])
        #expect(again.sidebarSpaces.first?.icon == "🌱")
        #expect(again.spaces.isEmpty)
        // Choosing it in the sidebar opens it.
        await again.selectSpace(at: url)
        #expect(again.selectedSpace?.url == url)
    }

    @Test func renamesAndSetsAnIcon() async throws {
        let model = makeModel()
        await model.createSpace(named: "Work", in: folder)
        let url = try #require(model.sidebarSpaces.first?.url)
        await model.renameSpace(at: url, to: "Job")
        #expect(model.selectedSpace?.name == "Job")
        #expect(model.sidebarSpaces.first?.name == "Job")
        #expect(SpaceLibrary(defaults: defaults).entries.first?.name == "Job")

        await model.setIcon("💼", forSpaceAt: url)
        #expect(model.selectedSpace?.icon == "💼")
        #expect(model.sidebarSpaces.first?.icon == "💼")
        await model.setIcon("Work", forSpaceAt: url)
        #expect(model.errorMessage != nil)
        #expect(model.sidebarSpaces.first?.icon == "💼")
        await model.setIcon(nil, forSpaceAt: url)
        #expect(model.sidebarSpaces.first?.icon == nil)
    }

    @Test func removingFromTheListKeepsTheFiles() async throws {
        let model = makeModel()
        await model.createSpace(named: "Work", in: folder)
        let url = try #require(model.sidebarSpaces.first?.url)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)

        await model.removeFromList(url)
        #expect(model.sidebarSpaces.isEmpty)
        #expect(model.spaces.isEmpty)
        #expect(model.selectedSpace == nil)
        #expect(FileManager.default.fileExists(atPath: url.path(percentEncoded: false)))
        #expect(SpaceLibrary(defaults: defaults).entries.isEmpty)
    }

    @Test func marksAndChangesTheDefaultSpace() async throws {
        let settings = MemoryKnodeSettings()
        let model = makeModel(settings: settings)
        await model.createSpace(named: "Work", in: folder, makeDefault: true)
        await model.createSpace(named: "Personal", in: folder)
        let (work, personal) = (model.sidebarSpaces[0], model.sidebarSpaces[1])
        #expect(work.isDefault && !personal.isDefault)
        #expect(try settings.defaultSpace() == work.url)

        model.setDefaultSpace(personal.url)
        #expect(model.sidebarSpaces.map(\.isDefault) == [false, true])
        #expect(try settings.defaultSpace() == personal.url)
        model.setDefaultSpace(nil)
        #expect(model.defaultSpaceURL == nil)
    }

    @Test func theFirstLaunchListsTheDefaultSpace() throws {
        let work = folder.appending(path: "Work.knode", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)
        try Data(#"{"format":0,"id":"01K83N6V4W5D2XQJ0Y7RZ9T1AB","name":"Work Notes"}"#.utf8).write(to: work.appending(path: "manifest.json"))

        let model = makeModel(settings: MemoryKnodeSettings(defaultSpace: work))
        #expect(model.sidebarSpaces.map(\.name) == ["Work Notes"])
        #expect(model.sidebarSpaces.first?.isDefault == true)
        // Removed once, it stays removed.
        SpaceLibrary(defaults: defaults).remove(work)
        #expect(makeModel(settings: MemoryKnodeSettings(defaultSpace: work)).sidebarSpaces.isEmpty)
    }

    @Test func theExampleSpaceIsListedButNotKept() async {
        let model = makeModel()
        await model.openExampleSpace()
        #expect(model.sidebarSpaces.map(\.isExample) == [true])
        #expect(model.selectedSpaceURL == SpacePanels.exampleURL)
        #expect(SpaceLibrary(defaults: defaults).entries.isEmpty)
    }

    // MARK: History of the open note

    @Test func theHistoryFollowsTheOpenNote() async throws {
        let model = makeModel()
        await model.createSpace(named: "Work", in: folder)
        await model.createNode()
        let editor = try #require(model.editor)
        editor.body = "First"
        await editor.save()
        editor.body = "Second"
        await editor.save()

        model.toggleHistory()
        #expect(model.isShowingHistory)
        let history = try #require(model.history)
        await history.load()
        #expect(history.versions.map(\.body) == ["Second", "First", ""])

        // Restoring puts the version in the editor.
        await history.restore(history.versions[1])
        #expect(model.editor?.body == "First")
        #expect(model.editor !== editor)
        #expect(model.nodes.first?.excerpt == "First")
    }

    @Test func unsavedEditsAreSavedBeforeAnUndo() async throws {
        let model = makeModel()
        await model.createSpace(named: "Work", in: folder)
        await model.createNode()
        let editor = try #require(model.editor)
        editor.body = "Kept"
        await editor.save()
        editor.body = "Typed, not saved yet"
        let history = try #require(model.history)
        await history.load()

        // The undo would now overwrite the typed text: it asks first.
        await history.undo(try #require(history.versions.first))
        #expect(history.conflict != nil)
        #expect(model.editor?.body == "Typed, not saved yet")
    }
}
