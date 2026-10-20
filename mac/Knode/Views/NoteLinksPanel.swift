import KnodeBrand
import SwiftUI

/// The links of the open note, beside it: links going out and backlinks, each with how it reads
/// from this note, and adding a typed link.
struct NoteLinksPanel: View {
    @Bindable var links: NoteLinksModel
    @Environment(AppModel.self) private var model
    @State private var isAdding = false

    var body: some View {
        List {
            Section {
                if links.outgoing.isEmpty {
                    Text("This note links to nothing yet.").foregroundStyle(.secondary)
                }
                ForEach(links.outgoingByLabel, id: \.label) { group in
                    LinkGroupRows(links: links, label: group.label, items: group.links)
                }
            } header: {
                Text("LINKS").font(.knodeLine(11))
            }
            Section {
                if links.backlinks.isEmpty {
                    Text("No note links here yet.").foregroundStyle(.secondary)
                }
                ForEach(links.backlinksByLabel, id: \.label) { group in
                    LinkGroupRows(links: links, label: group.label, items: group.links)
                }
            } header: {
                Text("BACKLINKS").font(.knodeLine(11))
            }
        }
        .listStyle(.sidebar)
        .safeAreaInset(edge: .bottom) {
            VStack(alignment: .leading, spacing: 6) {
                if let error = links.errorMessage {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(.red)
                }
                Button("Add Link…", systemImage: "link.badge.plus") { isAdding = true }
                    .help("Link this note to another, saying how they relate")
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(12)
        }
        .sheet(isPresented: $isAdding) {
            AddLinkSheet(links: links, notes: model.nodes.filter { $0.id != links.nodeID })
        }
        .task(id: links.nodeID) { await links.load() }
    }
}

/// The links that read with one label.
private struct LinkGroupRows: View {
    let links: NoteLinksModel
    let label: String
    let items: [NodeLinkInfo]
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
            ForEach(items) { link in
                Button {
                    Task { await model.openNode(link.nodeID) }
                } label: {
                    Label(link.title.isEmpty ? "Untitled" : link.title, systemImage: link.outgoing ? "arrow.up.right" : "arrow.down.left")
                        .lineLimit(1)
                }
                .buttonStyle(.link)
                .help(help(link))
                .contextMenu {
                    Button("Open", systemImage: "doc.text") { Task { await model.openNode(link.nodeID) } }
                    if links.canRemove(link) {
                        Button("Remove Link", systemImage: "link.badge.minus", role: .destructive) {
                            Task { await links.remove(link) }
                        }
                    }
                }
            }
        }
        .padding(.vertical, 2)
    }

    private func help(_ link: NodeLinkInfo) -> String {
        let made = link.kind == LinkKindInfo.mentions ? "From a [[link]] in the text" : "Linked by \(link.author.name)"
        return "\(made). Kind: \(link.kind)"
    }
}

/// Links the open note to another: choose the note and the kind, or define a new kind.
struct AddLinkSheet: View {
    let links: NoteLinksModel
    let notes: [NodeSummary]
    @Environment(\.dismiss) private var dismiss
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
        VStack(alignment: .leading, spacing: 14) {
            Text("Add Link").font(.title2.weight(.semibold))
            VStack(alignment: .leading, spacing: 6) {
                Text("To").font(.headline)
                TextField("Search notes", text: $query)
                    .textFieldStyle(.roundedBorder)
                List(shown, selection: $target) { note in
                    Label {
                        Text(note.title).lineLimit(1)
                    } icon: {
                        NodeGlyphView(kind: note.kind)
                    }
                    .tag(note.id)
                }
                .frame(height: 180)
            }
            VStack(alignment: .leading, spacing: 6) {
                Text("How they relate").font(.headline)
                Picker("Kind", selection: $isNewKind) {
                    Text("Existing Kind").tag(false)
                    Text("New Kind").tag(true)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                if isNewKind {
                    NewLinkKindForm(draft: $draft)
                } else {
                    Picker("Kind", selection: $kind) {
                        ForEach(links.choosableKinds) { kind in
                            Text(kindTitle(kind)).tag(kind.kind)
                        }
                    }
                    if let chosen = links.kinds.first(where: { $0.kind == kind }), !chosen.description.isEmpty {
                        Text(chosen.description).font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
            if let problem, target != nil || !isNewKind {
                Text(problem).font(.caption).foregroundStyle(.secondary)
            }
            HStack {
                Spacer()
                Button("Cancel", systemImage: "xmark", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Add Link", systemImage: "link") { Task { await save() } }
                    .keyboardShortcut(.defaultAction)
                    .disabled(problem != nil || isSaving)
            }
        }
        .padding(20)
        .frame(width: 440)
    }

    private func kindTitle(_ kind: LinkKindInfo) -> String {
        kind.direction == .symmetric || kind.inverseLabel.isEmpty ? kind.label : "\(kind.label) / \(kind.inverseLabel)"
    }

    private func save() async {
        guard let target else { return }
        isSaving = true
        defer { isSaving = false }
        let done = isNewKind ? await links.addLink(to: target, newKind: draft.kind) : await links.addLink(to: target, kind: kind)
        if done { dismiss() }
    }
}

/// The fields of a new kind of link.
struct NewLinkKindForm: View {
    @Binding var draft: LinkKindDraft

    var body: some View {
        Form {
            TextField("Label", text: $draft.label, prompt: Text("depends on"))
            Picker("Reads", selection: $draft.direction) {
                Text("One Way").tag(LinkDirection.directed)
                Text("Both Ways").tag(LinkDirection.symmetric)
            }
            .pickerStyle(.segmented)
            if draft.direction == .directed {
                TextField("Inverse", text: $draft.inverseLabel, prompt: Text("needed by"))
            }
            TextField("Name", text: $draft.name, prompt: Text(LinkKindInfo.name(fromLabel: draft.label).isEmpty ? "depends-on" : LinkKindInfo.name(fromLabel: draft.label)))
            TextField("Meaning", text: $draft.description, prompt: Text("Optional, for people and agents"))
        }
        .formStyle(.columns)
    }
}
