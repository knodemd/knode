import AppKit
import UniformTypeIdentifiers

extension UTType {
    /// A Knode space: a `.knode` package (declared in Info.plist).
    static let knodeSpace = UTType(exportedAs: "md.knode.space", conformingTo: .package)
}

/// The open and save panels for `.knode` packages.
@MainActor
enum SpacePanels {
    /// Where the example space appears to live. It is never written: its notes stay in memory.
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

    /// A folder, such as where a new space goes.
    static func chooseFolder(startingAt folder: URL) -> URL? {
        let panel = NSOpenPanel()
        panel.title = "Choose a Folder"
        panel.prompt = "Choose"
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        panel.directoryURL = folder
        return panel.runModal() == .OK ? panel.url : nil
    }
}
