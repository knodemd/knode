import AppKit
import Foundation
import Testing
@testable import Knode

/// Attachments in the Mac app: the core in the app, the note's model and how the body shows them.
@Suite @MainActor final class AttachmentTests {
    let folder: URL

    init() throws {
        folder = FileManager.default.temporaryDirectory.appending(path: "KnodeAttachmentTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    }

    deinit {
        try? FileManager.default.removeItem(at: folder)
    }

    @Test func attachesListsAndRemovesFilesThroughTheCore() async throws {
        let store = CoreSpaceStore(person: "Ada")
        let space = try await store.createSpace(at: folder.appending(path: "Files.knode", directoryHint: .isDirectory), name: "Files").id
        let note = try await store.createNode(in: space, title: "Trip", body: "")
        let file = folder.appending(path: "plan.txt")
        try Data("Day one".utf8).write(to: file)

        let model = NoteAttachmentsModel(spaceID: space, nodeID: note.id, store: store)
        let added = await model.attachFiles([file, folder])
        #expect(added.map(\.name) == ["plan.txt"])
        #expect(model.errorMessage?.contains("is a folder") == true)
        #expect(model.attachment(named: "plan.txt")?.author.name == "Ada")
        let url = try #require(model.file(of: "plan.txt"))
        #expect(try String(contentsOf: url, encoding: .utf8) == "Day one")

        let image = await model.attach(Data([0x89, 0x50, 0x4E, 0x47]), named: "Pasted image.png")
        #expect(image?.isImage == true)
        #expect(model.attachments.count == 2)

        await model.remove(try #require(model.attachment(named: "plan.txt")))
        #expect(model.attachments.map(\.name) == ["Pasted image.png"])
    }

    @Test func embedsGoAtTheEndOnLinesOfTheirOwn() {
        #expect(NoteAttachmentsModel.body("", embedding: ["a.png"]) == "![[a.png]]\n")
        #expect(NoteAttachmentsModel.body("Notes\n\n", embedding: ["a.png", "b.pdf"]) == "Notes\n\n![[a.png]]\n![[b.pdf]]\n")
        #expect(NoteAttachmentsModel.body("Notes", embedding: []) == "Notes")
    }

    @Test func embedLinesAreReadWithoutTheirSize() {
        #expect(MarkdownBlock.embeds("![[a.png]] ![[b.pdf|300]]") == ["a.png", "b.pdf"])
        #expect(MarkdownBlock.embeds("See ![[a.png]]") == nil)
        #expect(MarkdownBlock.embeds("[[a.png]]") == nil)
    }

    @Test func pastedImagesAreNamedByWhenAndReadAsPNG() throws {
        var parts = DateComponents()
        (parts.year, parts.month, parts.day, parts.hour, parts.minute, parts.second) = (2026, 10, 20, 9, 30, 5)
        let date = try #require(Calendar.current.date(from: parts))
        #expect(NoteAttachmentsModel.pastedImageName(at: date) == "Pasted image 2026-10-20 at 09.30.05.png")

        let image = NSImage(size: NSSize(width: 2, height: 2), flipped: false) { rect in
            NSColor.red.setFill()
            rect.fill()
            return true
        }
        let pasteboard = NSPasteboard(name: NSPasteboard.Name("KnodeAttachmentTests-\(UUID().uuidString)"))
        defer { pasteboard.releaseGlobally() }
        pasteboard.clearContents()
        pasteboard.setData(try #require(image.tiffRepresentation), forType: .tiff)
        let pasted = try #require(NoteAttachmentsModel.pastedImage(from: pasteboard, at: date))
        #expect(pasted.data.starts(with: [0x89, 0x50, 0x4E, 0x47]))
        #expect(pasted.name.hasSuffix(".png"))
    }

    @Test func droppedFilesAreWrittenWhereTheyWentOnLinesOfTheirOwn() {
        let textView = AttachingTextView()
        textView.string = "Before after"
        textView.insertEmbeds(["a.png"], at: NSRange(location: 6, length: 1))
        #expect(textView.string == "Before\n![[a.png]]\nafter")

        textView.string = "Line\n"
        textView.insertEmbeds(["a.png", "b.pdf"], at: NSRange(location: 5, length: 0))
        #expect(textView.string == "Line\n![[a.png]]\n![[b.pdf]]")

        textView.string = "Replace me"
        textView.insertEmbeds(["c.png"], at: NSRange(location: 0, length: 10))
        #expect(textView.string == "![[c.png]]")
    }

    @Test func withoutAttachmentsItReadsWhatATextViewReads() {
        let textView = AttachingTextView()
        let plain = NSTextView()
        #expect(textView.readablePasteboardTypes == plain.readablePasteboardTypes)
        #expect(textView.preferredPasteboardType(from: [.string, .png], restrictedToTypesFrom: nil) == plain.preferredPasteboardType(from: [.string, .png], restrictedToTypesFrom: nil))
        textView.attachments = AttachmentDrop(files: { _ in [] }, image: { _, _ in nil })
        #expect(textView.readablePasteboardTypes.first == .fileURL)
        #expect(textView.preferredPasteboardType(from: [.string, .png], restrictedToTypesFrom: nil) == .png)
    }
}
