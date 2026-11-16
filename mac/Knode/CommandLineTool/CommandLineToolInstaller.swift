import AppKit
import Foundation

/// Installs and uninstalls the `knode` command as `CommandLineTool` decides, asking for an
/// administrator's password when the folder needs one, and tells the person what happened.
@MainActor
final class CommandLineToolInstaller {
    /// Where the app made its link, so uninstalling removes only that one.
    static let installedLinkKey = "KnodeCommandLineToolLink"
    /// Set once the app has offered to install the command.
    static let offeredKey = "KnodeCommandLineToolOffered"
    /// The other copy of Knode the person chose to keep the command pointing at, so the app asks
    /// again only when the link points at a different one.
    static let keptStaleLinkKey = "KnodeCommandLineToolKeptStaleLink"

    let tool: CommandLineTool
    private let defaults: UserDefaults

    init(tool: CommandLineTool = .forThisApp, defaults: UserDefaults = .standard) {
        self.tool = tool
        self.defaults = defaults
    }

    var installedLink: URL? {
        defaults.string(forKey: Self.installedLinkKey).map { URL(filePath: $0) }
    }

    /// Whether the app made a link that is still there.
    var canUninstall: Bool {
        tool.uninstallPlan(installedLink: installedLink) != .nothing
    }

    // MARK: First run

    /// On the first run, offers to install the command, unless one is installed already.
    /// On every launch: when `knode` runs another copy of Knode, offers to point it at this one;
    /// otherwise, on the first run, offers to install it.
    func offerOnLaunch() {
        if case .staleLink(let link) = tool.status() {
            offerToRelink(link)
        } else {
            offerOnFirstRun()
        }
    }

    /// The command is a link into another copy of Knode, gone or elsewhere: agents and the
    /// terminal would keep running that copy's version. Asked once per other copy.
    private func offerToRelink(_ link: URL) {
        defaults.set(true, forKey: Self.offeredKey)
        let destination = tool.staleLinkDestination(link)
        let other = destination.flatMap(CommandLineTool.app(containing:))?.path(percentEncoded: false)
        let key = other ?? destination?.path(percentEncoded: false) ?? link.path(percentEncoded: false)
        guard defaults.string(forKey: Self.keptStaleLinkKey) != key else { return }
        let alert = NSAlert()
        alert.messageText = "Use this copy of Knode for the knode command?"
        let elsewhere = other.map { "another copy of Knode, at \($0)" } ?? "a copy of Knode that is no longer there"
        alert.informativeText = "\(link.path(percentEncoded: false)) runs the knode command inside \(elsewhere). "
            + "Claude Code, Codex and your terminal use that one until it points to this copy."
        alert.addButton(withTitle: "Use This Copy")
        alert.addButton(withTitle: "Keep It")
        if alert.runModal() == .alertFirstButtonReturn {
            install()
        } else {
            defaults.set(key, forKey: Self.keptStaleLinkKey)
        }
    }

    func offerOnFirstRun() {
        guard !defaults.bool(forKey: Self.offeredKey) else { return }
        defaults.set(true, forKey: Self.offeredKey)
        guard case .link = tool.installPlan() else { return }
        let alert = NSAlert()
        alert.messageText = "Install the knode command?"
        alert.informativeText = "With the knode command, Claude Code, Codex and your terminal can use Knode. "
            + "You can also install it later from the Knode menu."
        alert.addButton(withTitle: "Install")
        alert.addButton(withTitle: "Not Now")
        if alert.runModal() == .alertFirstButtonReturn { install() }
    }

    // MARK: Install and uninstall

    /// Installs the command, or says why there is nothing to do.
    func install() {
        switch tool.installPlan() {
        case .alreadyInstalled(let link):
            inform("The knode command is installed", "\(link.path(percentEncoded: false)) runs the knode command inside Knode.")
        case .useExisting(let url, let homebrew):
            let how = homebrew ? "with Homebrew" : "already"
            inform("The knode command is installed \(how)", "Knode uses \(url.path(percentEncoded: false)) and installs nothing.")
        case .link(let link, let needsAdministrator):
            do {
                try makeLink(at: link, needsAdministrator: needsAdministrator)
                defaults.set(link.path(percentEncoded: false), forKey: Self.installedLinkKey)
                inform("The knode command is installed", "Open a new terminal window and run knode --help.")
            } catch CommandLineToolError.cancelled {
                return
            } catch {
                fail("The knode command could not be installed", error, link: link)
            }
        }
    }

    /// Removes the link the app made, and nothing else.
    func uninstall() {
        switch tool.uninstallPlan(installedLink: installedLink) {
        case .nothing:
            defaults.removeObject(forKey: Self.installedLinkKey)
            inform("Nothing to uninstall", "Knode did not install the knode command, so it leaves it as it is.")
        case .remove(let link, let needsAdministrator):
            do {
                if needsAdministrator {
                    try runAsAdministrator("/bin/rm -f \(Self.quoted(link))")
                } else {
                    try tool.fileSystem.removeItem(at: link)
                }
                defaults.removeObject(forKey: Self.installedLinkKey)
                inform("The knode command is uninstalled", "\(link.path(percentEncoded: false)) was removed.")
            } catch CommandLineToolError.cancelled {
                return
            } catch {
                fail("The knode command could not be uninstalled", error, link: nil)
            }
        }
    }

    private func makeLink(at link: URL, needsAdministrator: Bool) throws {
        let binary = tool.bundledBinary
        if needsAdministrator {
            let folder = link.deletingLastPathComponent()
            try runAsAdministrator("/bin/mkdir -p \(Self.quoted(folder)) && /bin/ln -sfh \(Self.quoted(binary)) \(Self.quoted(link))")
        } else {
            // Only a stale link to Knode is ever replaced: the plan never points at anything else.
            if tool.fileSystem.destinationOfSymbolicLink(at: link) != nil { try tool.fileSystem.removeItem(at: link) }
            try tool.fileSystem.createSymbolicLink(at: link, pointingTo: binary)
        }
    }

    /// Runs a shell command as an administrator, after macOS asks for the password.
    private func runAsAdministrator(_ command: String) throws {
        let script = "do shell script \"\(Self.appleScriptEscaped(command))\" with administrator privileges"
        var error: NSDictionary?
        NSAppleScript(source: script)?.executeAndReturnError(&error)
        guard let error else { return }
        // -128: the person cancelled the password dialog.
        if (error[NSAppleScript.errorNumber] as? Int) == -128 { throw CommandLineToolError.cancelled }
        throw CommandLineToolError.failed(error[NSAppleScript.errorMessage] as? String ?? "unknown error")
    }

    // MARK: Messages

    private func inform(_ title: String, _ message: String) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.runModal()
    }

    private func fail(_ title: String, _ error: any Error, link: URL?) {
        var message = error.localizedDescription
        if let link {
            message += "\n\nTo install it yourself, run in Terminal:\nsudo ln -sf \(Self.quoted(tool.bundledBinary)) \(Self.quoted(link))"
        }
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = title
        alert.informativeText = message
        alert.runModal()
    }

    // MARK: Quoting

    /// A path quoted for the shell.
    nonisolated static func quoted(_ url: URL) -> String {
        "'" + url.path(percentEncoded: false).replacing("'", with: "'\\''") + "'"
    }

    /// Text made safe inside an AppleScript string literal.
    nonisolated static func appleScriptEscaped(_ text: String) -> String {
        text.replacing("\\", with: "\\\\").replacing("\"", with: "\\\"")
    }
}

enum CommandLineToolError: Error, LocalizedError {
    case cancelled
    case failed(String)

    var errorDescription: String? {
        switch self {
        case .cancelled: "Cancelled."
        case .failed(let message): message
        }
    }
}

extension CommandLineTool {
    /// The command inside this app, searched for where a shell would find it.
    static var forThisApp: CommandLineTool {
        CommandLineTool(
            bundledBinary: Bundle.main.bundleURL.appending(path: "Contents/Helpers/knode"),
            searchDirectories: searchDirectories(
                path: ProcessInfo.processInfo.environment["PATH"],
                userHome: FileManager.default.homeDirectoryForCurrentUser
            )
        )
    }
}

/// Offers to move Knode to the Applications folder when it runs from its disk image, Downloads or
/// any other folder, as `ApplicationLocation` decides, and opens the moved copy.
@MainActor
final class ApplicationMover {
    /// The folder the person chose to keep Knode in, so the app does not ask again for it.
    static let keptLocationKey = "KnodeKeptApplicationLocation"

    private let defaults: UserDefaults
    private let manager = FileManager.default

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// Asks, and moves the app if the person agrees. Returns true when the moved copy is opening
    /// and this one is about to quit.
    func offerToMove(bundle: URL = Bundle.main.bundleURL) -> Bool {
        let location = ApplicationLocation(bundle: bundle, home: manager.homeDirectoryForCurrentUser)
        let path = bundle.path(percentEncoded: false)
        guard location.shouldOfferMove, defaults.string(forKey: Self.keptLocationKey) != path else { return false }
        let alert = NSAlert()
        alert.messageText = "Move Knode to the Applications folder?"
        alert.informativeText = location == .diskImage || location == .translocated
            ? "Knode is running from its disk image. In Applications it stays when the image is ejected, "
                + "and the knode command your agents use keeps working."
            : "Knode is running from \(bundle.deletingLastPathComponent().path(percentEncoded: false)). "
                + "In Applications it stays put, and the knode command your agents use keeps working."
        alert.addButton(withTitle: "Move to Applications")
        alert.addButton(withTitle: "Do Not Move")
        guard alert.runModal() == .alertFirstButtonReturn else {
            defaults.set(path, forKey: Self.keptLocationKey)
            return false
        }
        let destination = URL(filePath: "/Applications").appending(path: bundle.lastPathComponent)
        do {
            if manager.fileExists(atPath: destination.path(percentEncoded: false)) {
                try manager.trashItem(at: destination, resultingItemURL: nil)
            }
            if location.removesOriginal {
                try manager.moveItem(at: bundle, to: destination)
            } else {
                try manager.copyItem(at: bundle, to: destination)
            }
        } catch {
            let failed = NSAlert()
            failed.messageText = "Knode could not be moved"
            failed.informativeText = "\(error.localizedDescription)\n\nDrag Knode to the Applications folder in Finder, then open it from there."
            failed.runModal()
            return false
        }
        NSWorkspace.shared.openApplication(at: destination, configuration: NSWorkspace.OpenConfiguration()) { _, _ in
            Task { @MainActor in NSApp.terminate(nil) }
        }
        return true
    }
}
