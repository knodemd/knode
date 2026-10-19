import AppKit
import UniformTypeIdentifiers

extension UTType {
    /// A Knode space: a `.knode` package (declared in Info.plist).
    static let knodeSpace = UTType(exportedAs: "md.knode.space", conformingTo: .package)
}

/// The open and save panels for `.knode` packages.
@MainActor
enum SpacePanels {
    /// A space that does not exist on disk, for trying the app with example notes.
    static let exampleURL = URL(filePath: NSTemporaryDirectory()).appending(path: "Example.knode")

    static func chooseSpace() -> URL? {
        let panel = NSOpenPanel()
        panel.title = "Open Space"
        panel.prompt = "Open"
        panel.allowedContentTypes = [.knodeSpace]
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.treatsFilePackagesAsDirectories = false
        return panel.runModal() == .OK ? panel.url : nil
    }

    static func chooseNewSpace() -> URL? {
        let panel = NSSavePanel()
        panel.title = "New Space"
        panel.prompt = "Create"
        panel.nameFieldStringValue = "My Space.knode"
        panel.allowedContentTypes = [.knodeSpace]
        panel.canCreateDirectories = true
        return panel.runModal() == .OK ? panel.url : nil
    }
}
