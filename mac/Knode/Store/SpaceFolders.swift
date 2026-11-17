import Foundation
import KnodeCore

/// A folder that feeds a space (section 15.2 of the format specification): a project whose agent
/// conversations are captured into it, a folder of Markdown notes, or a folder where an agent
/// keeps its sessions.
public struct SpaceFolder: Identifiable, Hashable, Sendable {
    /// The kinds of folder this version knows. A space may hold others, kept as they are.
    public enum Kind: String, CaseIterable, Sendable {
        case project, notes, agent
    }

    public var id: String
    /// `project`, `notes` or `agent`, or a kind of a newer version.
    public var kind: String
    /// The folder as the space records it: `~/...` inside the home folder, else absolute.
    public var path: String
    /// Whether the folder is captured (a project) or read (notes, agent).
    public var capture: Bool
    /// The rules of the folder, a JSON object, kept as they are when the folder changes.
    public var rules: String
    /// The folder on this machine, or `nil` when its path means nothing here.
    public var localURL: URL?
    /// The folder does not exist on this machine: it is kept and skipped.
    public var isMissing: Bool
    /// For a project folder, the project named after it that it holds the files of.
    /// `nil` for other folders, and for a project folder added before Knode made these.
    public var project: Project?

    /// The project a project folder holds the files of.
    public struct Project: Hashable, Sendable {
        public var id: Node.ID
        public var title: String
        /// Adding the folder just created it, rather than finding the one the space had.
        public var created: Bool
        /// The folder had been added before and already held this project.
        public var alreadyHeld: Bool
        /// The folder had been removed and, added again, took back the project it held before.
        public var heldBefore: Bool
        /// When adding the folder gave it a project named after its parent folder too, such as
        /// "api (b)", the project of its own name that another folder holds.
        public var passedOver: String?
        /// The folders, as recorded, that hold the files of `passedOver`, or, when
        /// `passedOverReleased`, the folder that held them last.
        public var passedOverFolders: [String]
        /// No folder holds `passedOver` now: it is kept for the folder that held it.
        public var passedOverReleased: Bool
        /// The other folders, as recorded, that hold the files of this project too.
        public var sharedWith: [String]

        public init(id: Node.ID, title: String, created: Bool = false, alreadyHeld: Bool = false, heldBefore: Bool = false, passedOver: String? = nil, passedOverFolders: [String] = [], passedOverReleased: Bool = false, sharedWith: [String] = []) {
            self.id = id
            self.title = title
            self.created = created
            self.alreadyHeld = alreadyHeld
            self.heldBefore = heldBefore
            self.passedOver = passedOver
            self.passedOverFolders = passedOverFolders
            self.passedOverReleased = passedOverReleased
            self.sharedWith = sharedWith
        }
    }

    public init(id: String, kind: String, path: String, capture: Bool, rules: String = "{}", localURL: URL? = nil, isMissing: Bool = false, project: Project? = nil) {
        self.id = id
        self.kind = kind
        self.path = path
        self.capture = capture
        self.rules = rules
        self.localURL = localURL
        self.isMissing = isMissing
        self.project = project
    }

    /// The title of the project adding a project folder at `url` makes or finds, from the core so
    /// it is the one the core creates: the folder's name, in NFC, with `[`, `]` and `|` replaced.
    /// `nil` for the home folder and the root of a disk, which have none.
    static func projectName(for url: URL) -> String? {
        KnodeCore.folderProjectTitle(path: ServiceSpaceStore.path(of: url))
    }

    public var knownKind: Kind? { Kind(rawValue: kind) }

    /// The agent whose sessions an agent folder holds (`rules.agent`).
    public var agent: String? {
        guard let data = rules.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }
        return object["agent"] as? String
    }

    /// The same folder with capture turned on or off, its rules unchanged.
    func draft(capture: Bool) -> FolderDraft {
        FolderDraft(id: id, kind: kind, path: path, capture: capture, rules: rules)
    }
}

/// A folder to add to a space, or a change to one.
public struct FolderDraft: Hashable, Sendable {
    /// The folder to change, or `nil` to add one (or change the one with the same kind and path).
    public var id: String?
    public var kind: String
    /// An absolute path, or one starting with `~/`.
    public var path: String
    public var capture: Bool
    /// The rules, a JSON object.
    public var rules: String

    public init(id: String? = nil, kind: String, path: String, capture: Bool = true, rules: String = "{}") {
        self.id = id
        self.kind = kind
        self.path = path
        self.capture = capture
        self.rules = rules
    }

    /// A new folder of this kind at `url`; an agent folder names the agent whose sessions it holds.
    static func adding(_ kind: SpaceFolder.Kind, at url: URL, agent: String? = nil) -> FolderDraft {
        var rules = "{}"
        if kind == .agent, let agent, let data = try? JSONSerialization.data(withJSONObject: ["agent": agent]) {
            rules = String(decoding: data, as: UTF8.self)
        }
        return FolderDraft(kind: kind.rawValue, path: ServiceSpaceStore.path(of: url), rules: rules)
    }
}

/// An agent whose conversations Knode captures, for the folders an agent keeps its sessions in.
struct CaptureAgent: Identifiable, Hashable, Sendable {
    /// Its name in provenance and rules, such as `claude-code`.
    var id: String
    /// Its name for people, such as Claude Code.
    var name: String

    /// The agents capture reads, from the core.
    static let all: [CaptureAgent] = KnodeCore.captureAgents().map { CaptureAgent(id: $0.id, name: $0.name) }

    /// The name for people of an agent, or the agent's own name when it is not one capture reads.
    static func name(of agent: String) -> String {
        all.first { $0.id == agent }?.name ?? agent
    }
}

/// The setting that turns capture into a space on or off.
enum CaptureSetting {
    static let key = "capture.enabled"
}
