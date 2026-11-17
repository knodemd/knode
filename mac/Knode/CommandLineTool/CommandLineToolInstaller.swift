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

    /// What to do once the command is installed on the first run: Connect your agents.
    private var onReadyOnFirstRun: (@MainActor () -> Void)?
    /// The command became ready on the first run before anyone asked to be told.
    private var readyOnFirstRun = false

    /// Runs `action` once the `knode` command is installed on the first run, or now if it already
    /// was. Never on a later run.
    func whenReadyOnFirstRun(_ action: @escaping @MainActor () -> Void) {
        if readyOnFirstRun {
            readyOnFirstRun = false
            action()
        } else {
            onReadyOnFirstRun = action
        }
    }

    private func readyOnThisFirstRun() {
        if let action = onReadyOnFirstRun {
            onReadyOnFirstRun = nil
            action()
        } else {
            readyOnFirstRun = true
        }
    }

    /// On the first run, offers to install the command, unless one is installed already. Once it
    /// is installed, or when it already was, the app goes on to Connect your agents.
    func offerOnFirstRun() {
        guard !defaults.bool(forKey: Self.offeredKey) else { return }
        defaults.set(true, forKey: Self.offeredKey)
        guard case .link = tool.installPlan() else { readyOnThisFirstRun(); return }
        let alert = NSAlert()
        alert.messageText = "Install the knode command?"
        alert.informativeText = "With the knode command, Claude Code, Codex and your terminal can use Knode. "
            + "You can also install it later from the Knode menu."
        alert.addButton(withTitle: "Install")
        alert.addButton(withTitle: "Not Now")
        if alert.runModal() == .alertFirstButtonReturn, install() { readyOnThisFirstRun() }
    }

    // MARK: Install and uninstall

    /// Installs the command, or says why there is nothing to do. Returns whether the command is
    /// installed afterwards.
    @discardableResult
    func install() -> Bool {
        switch tool.installPlan() {
        case .alreadyInstalled(let link):
            inform("The knode command is installed", "\(link.path(percentEncoded: false)) runs the knode command inside Knode.")
            return true
        case .useExisting(let url, let homebrew):
            let how = homebrew ? "with Homebrew" : "already"
            inform("The knode command is installed \(how)", "Knode uses \(url.path(percentEncoded: false)) and installs nothing.")
            return true
        case .link(let link, let needsAdministrator):
            do {
                try makeLink(at: link, needsAdministrator: needsAdministrator)
                defaults.set(link.path(percentEncoded: false), forKey: Self.installedLinkKey)
                inform("The knode command is installed", "Open a new terminal window and run knode --help.")
                return true
            } catch CommandLineToolError.cancelled {
                return false
            } catch {
                fail("The knode command could not be installed", error, link: link)
                return false
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
