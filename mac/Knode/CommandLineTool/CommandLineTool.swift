import Foundation

/// Where the `knode` command is, and what installing or uninstalling it means.
///
/// The app carries `knode` inside, at `Knode.app/Contents/Helpers/knode`. Installing the command
/// makes a symbolic link to it in `/usr/local/bin`, or in `/opt/homebrew/bin` when only that one is
/// writable, or in `/usr/local/bin` with an administrator's password. A `knode` installed another
/// way, such as with Homebrew, is used as it is and never replaced. Uninstalling removes only the
/// link the app made.
///
/// Every decision is made here, against a `FileSystem`, so it can be tested; carrying it out is
/// `CommandLineToolInstaller`'s job.
public struct CommandLineTool: Sendable {
    /// Where a link is made, best first.
    public static let linkDirectories = [URL(filePath: "/usr/local/bin"), URL(filePath: "/opt/homebrew/bin")]

    /// The `knode` inside this app.
    public var bundledBinary: URL
    /// The folders searched for a `knode` command, in order.
    public var searchDirectories: [URL]
    public var fileSystem: any FileSystem

    public init(bundledBinary: URL, searchDirectories: [URL], fileSystem: any FileSystem = LocalFileSystem()) {
        self.bundledBinary = bundledBinary
        self.searchDirectories = searchDirectories
        self.fileSystem = fileSystem
    }

    /// The folders a shell usually finds commands in, then the `PATH` of this process. Apps start
    /// with a short `PATH`, so the usual folders come first.
    public static func searchDirectories(path: String?, userHome: URL) -> [URL] {
        var directories = [
            URL(filePath: "/opt/homebrew/bin"),
            URL(filePath: "/usr/local/bin"),
            userHome.appending(path: ".local/bin"),
            userHome.appending(path: ".cargo/bin"),
        ]
        for entry in (path ?? "").split(separator: ":") where !entry.isEmpty {
            let url = URL(filePath: String(entry))
            if !directories.contains(url) { directories.append(url) }
        }
        return directories
    }

    // MARK: Status

    public enum Status: Equatable, Sendable {
        /// `knode` is a link to the command inside this app.
        case linked(URL)
        /// `knode` is a link to the command inside a copy of Knode that is gone or elsewhere.
        case staleLink(URL)
        /// `knode` was installed another way; `homebrew` when Homebrew installed it.
        case external(URL, homebrew: Bool)
        case notInstalled
    }

    /// The first `knode` found in the search folders.
    public func status() -> Status {
        let bundled = fileSystem.resolvingSymbolicLinks(bundledBinary)
        for directory in searchDirectories {
            let candidate = directory.appending(path: "knode")
            if let destination = fileSystem.destinationOfSymbolicLink(at: candidate) {
                if fileSystem.resolvingSymbolicLinks(destination) == bundled { return .linked(candidate) }
                if Self.isInsideKnodeApp(destination) { return .staleLink(candidate) }
            }
            if fileSystem.isExecutableFile(at: candidate) {
                return .external(candidate, homebrew: isHomebrew(candidate))
            }
        }
        return .notInstalled
    }

    /// The `knode` the app runs for Knode Service: an installed one if there is one, else the one
    /// inside the app.
    public func executable() -> URL {
        switch status() {
        case .external(let url, _), .linked(let url): url
        case .staleLink, .notInstalled: bundledBinary
        }
    }

    // MARK: Install

    public enum InstallPlan: Equatable, Sendable {
        /// The command is already a link to this app.
        case alreadyInstalled(URL)
        /// Another `knode` is installed: the app uses it and installs nothing.
        case useExisting(URL, homebrew: Bool)
        /// Make a link at `link`, replacing a stale one; `needsAdministrator` when the folder is
        /// not writable.
        case link(URL, needsAdministrator: Bool)
    }

    public func installPlan() -> InstallPlan {
        switch status() {
        case .linked(let url):
            return .alreadyInstalled(url)
        case .external(let url, let homebrew):
            return .useExisting(url, homebrew: homebrew)
        case .staleLink(let url):
            return .link(url, needsAdministrator: !fileSystem.isWritableDirectory(at: url.deletingLastPathComponent()))
        case .notInstalled:
            if let directory = Self.linkDirectories.first(where: fileSystem.isWritableDirectory) {
                return .link(directory.appending(path: "knode"), needsAdministrator: false)
            }
            return .link(Self.linkDirectories[0].appending(path: "knode"), needsAdministrator: true)
        }
    }

    // MARK: Uninstall

    public enum UninstallPlan: Equatable, Sendable {
        /// Remove the link the app made.
        case remove(URL, needsAdministrator: Bool)
        /// The app made no link that is still there: nothing to remove.
        case nothing
    }

    /// - Parameter installedLink: where the app made its link, as it remembered.
    public func uninstallPlan(installedLink: URL?) -> UninstallPlan {
        guard let link = installedLink,
              let destination = fileSystem.destinationOfSymbolicLink(at: link),
              Self.isInsideKnodeApp(destination)
        else { return .nothing }
        return .remove(link, needsAdministrator: !fileSystem.isWritableDirectory(at: link.deletingLastPathComponent()))
    }

    // MARK: Helpers

    /// Whether `url` is the `knode` inside a Knode app.
    static func isInsideKnodeApp(_ url: URL) -> Bool {
        let components = url.standardizedFileURL.pathComponents
        guard components.count >= 4 else { return false }
        return components.suffix(3) == ["Contents", "Helpers", "knode"] && components[components.count - 4].hasSuffix(".app")
    }

    private func isHomebrew(_ url: URL) -> Bool {
        let path = fileSystem.resolvingSymbolicLinks(url).path(percentEncoded: false)
        return path.contains("/Cellar/") || path.contains("/Caskroom/") || path.hasPrefix("/opt/homebrew/")
            || path.hasPrefix("/home/linuxbrew/")
    }
}
