import SwiftUI

/// The links of the open note, beside it: links going out and backlinks, each with how it reads
/// from this note, and adding a typed link.
struct NoteLinksPanel: View {
    @Bindable var links: NoteLinksModel
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme
    @State private var isAdding = false

    var body: some View {
        VStack(spacing: 0) {
            KScrollView {
                VStack(alignment: .leading, spacing: KSpace.x6) {
                    section("Links", count: links.outgoing.count, empty: "This note links to nothing yet.", groups: links.outgoingByLabel)
                    section("Backlinks", count: links.backlinks.count, empty: "No note links here yet.", groups: links.backlinksByLabel)
                }
                .padding(KSpace.x4)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            InspectorFooter(error: links.errorMessage) {
                Button { isAdding = true } label: {
                    HStack(spacing: KSpace.x2) {
                        KIcon(.link, size: .sm)
                        Text("Add link")
                    }
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(KButtonStyle(kind: .secondary))
                .kTooltip("Link this note to another, saying how they relate")
                .kPopover(isPresented: $isAdding, edge: .leading) {
                    AddLinkCard(links: links, notes: model.nodes.filter { $0.id != links.nodeID }) { isAdding = false }
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(theme.panel)
        .task(id: links.nodeID) { await links.load() }
        .knodeTheme()
    }

    private func section(_ title: String, count: Int, empty: String, groups: [(label: String, links: [NodeLinkInfo])]) -> some View {
        VStack(alignment: .leading, spacing: KSpace.x2) {
            KSectionLabel(title) {
                if count > 0 { Text("\(count)").kLineStyle(10).foregroundStyle(theme.muted).monospacedDigit() }
            }
            if groups.isEmpty {
                Text(empty).font(KFont.ui(12)).foregroundStyle(theme.muted).padding(.vertical, KSpace.x1)
            }
            ForEach(groups, id: \.label) { group in
                LinkGroupRows(links: links, label: group.label, items: group.links)
            }
        }
    }
}

/// The bottom of an inspector panel: an error, if any, over its actions, on a hairline.
struct InspectorFooter<Actions: View>: View {
    var error: String?
    @ViewBuilder var actions: Actions
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x2) {
            if let error {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    KIcon(.warning, size: .xs, color: theme.danger)
                    Text(error).font(KFont.ui(12)).foregroundStyle(theme.danger).fixedSize(horizontal: false, vertical: true)
                }
            }
            actions
        }
        .padding(KSpace.x4)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .top) { Rectangle().fill(theme.line).frame(height: 1) }
    }
}

/// The links that read with one label.
private struct LinkGroupRows: View {
    let links: NoteLinksModel
    let label: String
    let items: [NodeLinkInfo]
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .font(KFont.ui(12, .medium))
                .foregroundStyle(theme.muted)
                .padding(.horizontal, KSpace.x3)
                .padding(.top, KSpace.x1)
            ForEach(items) { link in
                KRow(action: { Task { await model.openNode(link.nodeID) } }) {
                    KIcon(link.outgoing ? .forward : .back, size: .xs,
                          color: link.kind == LinkKindInfo.mentions ? theme.muted : theme.indigo)
                    Text(link.title.isEmpty ? "Untitled" : link.title)
                        .font(KFont.ui(13))
                        .foregroundStyle(theme.fg)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                }
                .kTooltip(help(link))
                .kContextMenu {
                    var entries: [KMenuEntry] = [.action("Open", icon: .note) { Task { await model.openNode(link.nodeID) } }]
                    if links.canRemove(link) {
                        entries.append(.divider)
                        entries.append(.action("Remove link", icon: .delete, isDestructive: true) { Task { await links.remove(link) } })
                    }
                    return entries
                }
            }
        }
    }

    private func help(_ link: NodeLinkInfo) -> String {
        let made = link.kind == LinkKindInfo.mentions ? "From a [[link]] in the text" : "Linked by \(link.author.name)"
        return "\(made). Kind: \(link.kind)"
    }
}

/// Links the open note to another: choose the note and the kind, or define a new kind.
struct AddLinkCard: View {
    let links: NoteLinksModel
    let notes: [NodeSummary]
    var close: () -> Void
    @Environment(\.kTheme) private var theme
    @State private var query = ""
    @State private var target: Node.ID?
    @State private var kind = LinkKindInfo.related
    @State private var isNewKind = false
    @State private var draft = LinkKindDraft()
    @State private var isSaving = false

    private var shown: [NodeSummary] {
        let query = query.trimmingCharacters(in: .whitespaces)
        return query.isEmpty ? notes : notes.filter { $0.title.localizedCaseInsensitiveContains(query) }
    }

    private var problem: String? {
        if target == nil { return "Choose the note to link to." }
        return isNewKind ? draft.problem(existing: links.kinds) : nil
    }

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x4) {
            KSheetHeader(kicker: "Links", title: "Add link")
            VStack(alignment: .leading, spacing: KSpace.x2) {
                KSectionLabel("To")
                KSearchField(placeholder: "Search notes", text: $query, size: .small)
                KScrollView {
                    LazyVStack(alignment: .leading, spacing: 1) {
                        ForEach(shown) { note in
                            KRow(isSelected: target == note.id, action: { target = note.id }) {
                                NodeGlyphView(kind: note.kind)
                                Text(note.title.isEmpty ? "Untitled" : note.title).font(KFont.ui(13)).foregroundStyle(theme.fg).lineLimit(1)
                            }
                        }
                    }
                }
                .frame(height: 168)
            }
            VStack(alignment: .leading, spacing: KSpace.x2) {
                KSectionLabel("How they relate")
                KSegmented(selection: $isNewKind, options: [KOption(false, "Existing kind"), KOption(true, "New kind")])
                if isNewKind {
                    NewLinkKindForm(draft: $draft)
                } else {
                    FlowRow(spacing: 6) {
                        ForEach(links.choosableKinds) { option in
                            Button { kind = option.kind } label: {
                                KChip(kindTitle(option), kind: option.kind == kind ? .solid : .neutral)
                            }
                            .buttonStyle(KPlainButtonStyle())
                            .accessibilityAddTraits(option.kind == kind ? .isSelected : [])
                        }
                    }
                    if let chosen = links.kinds.first(where: { $0.kind == kind }), !chosen.description.isEmpty {
                        Text(chosen.description).font(KFont.ui(12)).foregroundStyle(theme.muted).fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            if let problem, target != nil || !isNewKind {
                Text(problem).font(KFont.ui(12)).foregroundStyle(theme.muted)
            }
            HStack(spacing: KSpace.x2) {
                Spacer()
                KButton("Cancel", kind: .ghost, size: .small, action: close)
                Button { Task { await save() } } label: {
                    HStack(spacing: 6) {
                        KIcon(.link, size: .sm)
                        Text("Add link")
                    }
                }
                .buttonStyle(KButtonStyle(kind: .primary, size: .small))
                .keyboardShortcut(.defaultAction)
                .disabled(problem != nil || isSaving)
            }
        }
        .frame(width: 360)
    }

    private func kindTitle(_ kind: LinkKindInfo) -> String {
        kind.direction == .symmetric || kind.inverseLabel.isEmpty ? kind.label : "\(kind.label) / \(kind.inverseLabel)"
    }

    private func save() async {
        guard let target else { return }
        isSaving = true
        defer { isSaving = false }
        let done = isNewKind ? await links.addLink(to: target, newKind: draft.kind) : await links.addLink(to: target, kind: kind)
        if done { close() }
    }
}

/// The fields of a new kind of link.
struct NewLinkKindForm: View {
    @Binding var draft: LinkKindDraft
    @Environment(\.kTheme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x2) {
            field("Label") { KTextField("depends on", text: $draft.label, size: .small) }
            field("Reads") {
                KSegmented(selection: $draft.direction, options: [KOption(LinkDirection.directed, "One way"), KOption(LinkDirection.symmetric, "Both ways")])
            }
            if draft.direction == .directed {
                field("Inverse") { KTextField("needed by", text: $draft.inverseLabel, size: .small) }
            }
            let suggested = LinkKindInfo.name(fromLabel: draft.label)
            field("Name") { KTextField(suggested.isEmpty ? "depends-on" : suggested, text: $draft.name, mono: true, size: .small) }
            field("Meaning") { KTextField("Optional, for people and agents", text: $draft.description, size: .small) }
        }
    }

    private func field<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        HStack(spacing: KSpace.x2) {
            Text(title).font(KFont.ui(12)).foregroundStyle(theme.muted).frame(width: 64, alignment: .leading)
            content()
        }
    }
}
