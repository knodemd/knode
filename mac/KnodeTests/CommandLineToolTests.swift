import Foundation
import Synchronization
import Testing
@testable import Knode

/// The decisions behind installing the `knode` command, against a file system in memory.
@Suite struct CommandLineToolTests {
    /// Files, folders and symbolic links in memory.
    final class MemoryFileSystem: FileSystem {
        struct State {
            var executables: Set<String> = []
            var writableFolders: Set<String> = []
            var links: [String: String] = [:]
        }

        let state: Mutex<State>

        init(executables: Set<String> = [], writableFolders: Set<String> = [], links: [String: String] = [:]) {
            state = Mutex(State(executables: executables, writableFolders: writableFolders, links: links))
        }

        func isExecutableFile(at url: URL) -> Bool {
            state.withLock { $0.executables.contains(resolve(url.path, in: $0)) }
        }

        func isWritableDirectory(at url: URL) -> Bool {
            state.withLock { $0.writableFolders.contains(url.path) }
        }

        func destinationOfSymbolicLink(at url: URL) -> URL? {
            state.withLock { $0.links[url.path].map { URL(filePath: $0) } }
        }

        func resolvingSymbolicLinks(_ url: URL) -> URL {
            URL(filePath: state.withLock { resolve(url.path, in: $0) })
        }

        func createSymbolicLink(at url: URL, pointingTo destination: URL) throws {
            state.withLock { $0.links[url.path] = destination.path }
        }

        func removeItem(at url: URL) throws {
            state.withLock { $0.links[url.path] = nil }
        }

        private func resolve(_ path: String, in state: State) -> String {
            var path = path
            while let next = state.links[path] { path = next }
            return path
        }
    }

    static let app = "/Applications/Knode.app/Contents/Helpers/knode"
    static let directories = CommandLineTool.searchDirectories(path: "/usr/bin:/bin", userHome: URL(filePath: "/Users/ada"))

    func tool(_ fileSystem: MemoryFileSystem) -> CommandLineTool {
        CommandLineTool(bundledBinary: URL(filePath: Self.app), searchDirectories: Self.directories, fileSystem: fileSystem)
    }

    @Test func searchesTheUsualFoldersThenThePath() {
        #expect(Self.directories.map(\.path) == [
            "/opt/homebrew/bin", "/usr/local/bin", "/Users/ada/.local/bin", "/Users/ada/.cargo/bin", "/usr/bin", "/bin",
        ])
    }

    @Test func linksInUsrLocalBinWhenItIsWritable() {
        let fileSystem = MemoryFileSystem(executables: [Self.app], writableFolders: ["/usr/local/bin", "/opt/homebrew/bin"])
        #expect(tool(fileSystem).status() == .notInstalled)
        #expect(tool(fileSystem).installPlan() == .link(URL(filePath: "/usr/local/bin/knode"), needsAdministrator: false))
    }

    @Test func linksInHomebrewBinWhenOnlyThatIsWritable() {
        let fileSystem = MemoryFileSystem(executables: [Self.app], writableFolders: ["/opt/homebrew/bin"])
        #expect(tool(fileSystem).installPlan() == .link(URL(filePath: "/opt/homebrew/bin/knode"), needsAdministrator: false))
    }

    @Test func asksForAnAdministratorWhenNothingIsWritable() {
        let fileSystem = MemoryFileSystem(executables: [Self.app])
        #expect(tool(fileSystem).installPlan() == .link(URL(filePath: "/usr/local/bin/knode"), needsAdministrator: true))
    }

    @Test func usesKnodeFromHomebrew() {
        let cellar = "/opt/homebrew/Cellar/knode/0.1.0/bin/knode"
        let fileSystem = MemoryFileSystem(
            executables: [Self.app, cellar],
            writableFolders: ["/opt/homebrew/bin"],
            links: ["/opt/homebrew/bin/knode": cellar]
        )
        let homebrew = URL(filePath: "/opt/homebrew/bin/knode")
        #expect(tool(fileSystem).status() == .external(homebrew, homebrew: true))
        #expect(tool(fileSystem).installPlan() == .useExisting(homebrew, homebrew: true))
        #expect(tool(fileSystem).executable() == homebrew)
    }

    @Test func usesAKnodeInstalledAnotherWay() {
        let fileSystem = MemoryFileSystem(executables: [Self.app, "/Users/ada/.local/bin/knode"], writableFolders: ["/usr/local/bin"])
        let installed = URL(filePath: "/Users/ada/.local/bin/knode")
        #expect(tool(fileSystem).installPlan() == .useExisting(installed, homebrew: false))
    }

    @Test func knowsItsOwnLink() {
        let fileSystem = MemoryFileSystem(executables: [Self.app], writableFolders: ["/usr/local/bin"], links: ["/usr/local/bin/knode": Self.app])
        let link = URL(filePath: "/usr/local/bin/knode")
        #expect(tool(fileSystem).installPlan() == .alreadyInstalled(link))
        #expect(tool(fileSystem).executable() == link)
    }

    @Test func replacesALinkToAKnodeAppThatMoved() {
        let fileSystem = MemoryFileSystem(
            executables: [Self.app],
            links: ["/usr/local/bin/knode": "/Users/ada/Downloads/Knode.app/Contents/Helpers/knode"]
        )
        #expect(tool(fileSystem).status() == .staleLink(URL(filePath: "/usr/local/bin/knode")))
        #expect(tool(fileSystem).installPlan() == .link(URL(filePath: "/usr/local/bin/knode"), needsAdministrator: true))
        #expect(tool(fileSystem).executable() == URL(filePath: Self.app))
    }

    @Test func usesTheBundledCommandWhenNoneIsInstalled() {
        let fileSystem = MemoryFileSystem(executables: [Self.app])
        #expect(tool(fileSystem).executable() == URL(filePath: Self.app))
    }

    @Test func uninstallsOnlyTheLinkItMade() {
        let link = URL(filePath: "/usr/local/bin/knode")
        let fileSystem = MemoryFileSystem(executables: [Self.app], writableFolders: ["/usr/local/bin"], links: [link.path: Self.app])
        #expect(tool(fileSystem).uninstallPlan(installedLink: link) == .remove(link, needsAdministrator: false))
        // A link it never made, or one that now points elsewhere, stays.
        #expect(tool(fileSystem).uninstallPlan(installedLink: nil) == .nothing)
        try? fileSystem.createSymbolicLink(at: link, pointingTo: URL(filePath: "/opt/homebrew/Cellar/knode/0.1.0/bin/knode"))
        #expect(tool(fileSystem).uninstallPlan(installedLink: link) == .nothing)
    }

    @Test func uninstallNeedsAnAdministratorWhenTheFolderIsNotWritable() {
        let link = URL(filePath: "/usr/local/bin/knode")
        let fileSystem = MemoryFileSystem(executables: [Self.app], links: [link.path: Self.app])
        #expect(tool(fileSystem).uninstallPlan(installedLink: link) == .remove(link, needsAdministrator: true))
    }

    @Test func recognisesTheCommandInsideAKnodeApp() {
        #expect(CommandLineTool.isInsideKnodeApp(URL(filePath: Self.app)))
        #expect(CommandLineTool.isInsideKnodeApp(URL(filePath: "/tmp/Build/Knode Dev.app/Contents/Helpers/knode")))
        #expect(!CommandLineTool.isInsideKnodeApp(URL(filePath: "/opt/homebrew/bin/knode")))
        #expect(!CommandLineTool.isInsideKnodeApp(URL(filePath: "/Contents/Helpers/knode")))
    }

    @Test func quotesPathsForTheShellAndAppleScript() {
        #expect(CommandLineToolInstaller.quoted(URL(filePath: "/Applications/Ada's Knode.app")) == "'/Applications/Ada'\\''s Knode.app'")
        #expect(CommandLineToolInstaller.appleScriptEscaped(#"ln "a\b""#) == #"ln \"a\\b\""#)
    }

    @Test func knodeHomeFollowsTheEnvironment() {
        let home = URL(filePath: "/Users/ada", directoryHint: .isDirectory)
        #expect(KnodeHome.resolve(environment: ["KNODE_HOME": "/tmp/kh"], userHome: home).socketURL.path == "/tmp/kh/service.sock")
        #expect(KnodeHome.resolve(environment: [:], userHome: home).socketURL.path == "/Users/ada/Library/Application Support/Knode/service.sock")
    }
}
