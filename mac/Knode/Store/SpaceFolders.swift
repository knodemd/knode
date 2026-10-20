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

    public init(id: String, kind: String, path: String, capture: Bool, rules: String = "{}", localURL: URL? = nil, isMissing: Bool = false) {
        self.id = id
        self.kind = kind
        self.path = path
        self.capture = capture
        self.rules = rules
        self.localURL = localURL
        self.isMissing = isMissing
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
