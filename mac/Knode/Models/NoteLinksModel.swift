import Foundation
import Observation

/// The links of one note, as its inspector shows them: going out and coming in, each with the
/// label it reads with from the note, and adding a typed link, with a kind of the space's own if
/// needed.
@MainActor
@Observable
final class NoteLinksModel {
    let spaceID: Space.ID
    let nodeID: Node.ID

    private(set) var outgoing: [NodeLinkInfo] = []
    private(set) var backlinks: [NodeLinkInfo] = []
    private(set) var kinds: [LinkKindInfo] = []
    private(set) var isLoaded = false
    var errorMessage: String?

    @ObservationIgnored private let store: any MapStore
    /// Called after a link is made or removed, so other views can show it.
    @ObservationIgnored var didChange: (@MainActor () async -> Void)?

    init(spaceID: Space.ID, nodeID: Node.ID, store: any MapStore) {
        self.spaceID = spaceID
        self.nodeID = nodeID
        self.store = store
    }

    /// The kinds a person can choose for a new link: every kind but `mentions`, which comes only
    /// from `[[links]]` in a body.
    var choosableKinds: [LinkKindInfo] {
        kinds.filter { $0.kind != LinkKindInfo.mentions }
    }

    /// Links going out, grouped by the label they read with, in order.
    var outgoingByLabel: [(label: String, links: [NodeLinkInfo])] {
        Self.grouped(outgoing)
    }

    var backlinksByLabel: [(label: String, links: [NodeLinkInfo])] {
        Self.grouped(backlinks)
    }

    static func grouped(_ links: [NodeLinkInfo]) -> [(label: String, links: [NodeLinkInfo])] {
        var groups: [(label: String, links: [NodeLinkInfo])] = []
        for link in links {
            if let index = groups.firstIndex(where: { $0.label == link.label }) {
                groups[index].links.append(link)
            } else {
                groups.append((link.label, [link]))
            }
        }
        return groups
    }

    func load() async {
        do {
            async let links = store.links(of: nodeID, in: spaceID)
            async let kinds = store.linkKinds(in: spaceID)
            let all = try await links
            self.kinds = try await kinds
            outgoing = all.filter(\.outgoing)
            backlinks = all.filter { !$0.outgoing }
            isLoaded = true
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// Links this note to another with a kind. Returns whether it worked.
    @discardableResult
    func addLink(to target: Node.ID, kind: String) async -> Bool {
        do {
            try await store.link(from: nodeID, to: target, kind: kind, in: spaceID)
            await load()
            await didChange?()
            return true
        } catch {
            errorMessage = error.localizedDescription
            return false
        }
    }

    /// Defines a new kind of link, then links this note to another with it.
    @discardableResult
    func addLink(to target: Node.ID, newKind: LinkKindInfo) async -> Bool {
        do {
            let kind = try await store.setLinkKind(newKind, in: spaceID)
            return await addLink(to: target, kind: kind.kind)
        } catch {
            errorMessage = error.localizedDescription
            return false
        }
    }

    /// Whether a link can be removed here: links made by hand. `mentions` links go away with the
    /// `[[link]]` in the body.
    func canRemove(_ link: NodeLinkInfo) -> Bool {
        link.kind != LinkKindInfo.mentions
    }

    func remove(_ link: NodeLinkInfo) async {
        guard canRemove(link) else { return }
        do {
            try await store.unlink(from: link.from, to: link.to, kind: link.kind, in: spaceID)
            await load()
            await didChange?()
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}

/// A new kind of link, as a person defines it.
struct LinkKindDraft: Equatable {
    var label = ""
    var inverseLabel = ""
    var direction: LinkDirection = .directed
    var description = ""
    /// The name stored on links; made from the label unless typed.
    var name = ""

    var effectiveName: String {
        let typed = name.trimmingCharacters(in: .whitespaces)
        return typed.isEmpty ? LinkKindInfo.name(fromLabel: label) : typed
    }

    /// Why the kind cannot be defined as it is, or nil when it can.
    func problem(existing: [LinkKindInfo]) -> String? {
        let label = label.trimmingCharacters(in: .whitespacesAndNewlines)
        if label.isEmpty { return "A kind needs a label, such as “depends on”." }
        if label.count > 64 || inverseLabel.count > 64 { return "Labels are at most 64 characters." }
        let name = effectiveName
        if !LinkKindInfo.isValidName(name) {
            return "The name is lower-case letters, digits and hyphens, starting with a letter."
        }
        if existing.contains(where: { $0.kind == name }) { return "A kind named \(name) already exists." }
        return nil
    }

    var kind: LinkKindInfo {
        LinkKindInfo(
            kind: effectiveName,
            label: label.trimmingCharacters(in: .whitespacesAndNewlines),
            inverseLabel: direction == .symmetric ? "" : inverseLabel.trimmingCharacters(in: .whitespacesAndNewlines),
            direction: direction,
            description: description.trimmingCharacters(in: .whitespacesAndNewlines)
        )
    }
}
