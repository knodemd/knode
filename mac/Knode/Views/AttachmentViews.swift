import AppKit
import KnodeBrand
import PDFKit
import SwiftUI
import UniformTypeIdentifiers

/// The files attached to the open note, beside it: open one, show it in the Finder or remove it,
/// and attach more by dropping files on the list or choosing them.
struct NoteAttachmentsPanel: View {
    @Bindable var attachments: NoteAttachmentsModel
    @Environment(AppModel.self) private var model
    @State private var isChoosing = false
    @State private var isTargeted = false

    var body: some View {
        List {
            Section {
                if attachments.isLoaded, attachments.attachments.isEmpty {
                    Text("Drop files on the note, or paste an image, to attach them.")
                        .foregroundStyle(.secondary)
                }
                ForEach(attachments.attachments) { attachment in
                    AttachmentRow(attachments: attachments, attachment: attachment)
                }
            } header: {
                Text("ATTACHMENTS").font(.knodeLine(11))
            }
        }
        .listStyle(.sidebar)
        .overlay {
            if isTargeted { DropHighlight() }
        }
        .onDrop(of: [.fileURL], isTargeted: $isTargeted) { providers in
            Task { await model.attachFilesAtEnd(await DroppedItems.fileURLs(providers)) }
            return true
        }
        .safeAreaInset(edge: .bottom) {
            VStack(alignment: .leading, spacing: 6) {
                if let error = attachments.errorMessage {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(.red)
                }
                Button("Attach Files…", systemImage: "paperclip") { isChoosing = true }
                    .help("Attach files to this note; each shows where the note ends")
                    .disabled(attachments.isAttaching)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(12)
        }
        .fileImporter(isPresented: $isChoosing, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            guard case .success(let urls) = result else { return }
            Task { await model.attachFilesAtEnd(urls) }
        }
        .task(id: attachments.nodeID) { await attachments.load() }
    }
}

/// One attached file in the inspector.
private struct AttachmentRow: View {
    let attachments: NoteAttachmentsModel
    let attachment: AttachmentInfo

    var body: some View {
        HStack(spacing: 8) {
            FileIcon(url: attachments.file(of: attachment.name), type: attachment.contentType)
                .frame(width: 28, height: 28)
            VStack(alignment: .leading, spacing: 2) {
                Text(attachment.name)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text("\(attachment.size.formatted(.byteCount(style: .file))), \(attachment.author.name)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .contentShape(Rectangle())
        .onTapGesture(count: 2) { attachments.open(attachment) }
        .help("\(attachment.name), attached \(attachment.createdAt.formatted(date: .abbreviated, time: .shortened))")
        .contextMenu {
            Button("Open", systemImage: "arrow.up.forward.app") { attachments.open(attachment) }
            Button("Show in Finder", systemImage: "folder") { attachments.reveal(attachment) }
            Button("Copy Embed", systemImage: "doc.on.doc") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(attachment.embed, forType: .string)
            }
            Divider()
            Button("Remove Attachment", systemImage: "trash", role: .destructive) {
                Task { await attachments.remove(attachment) }
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityAction(named: "Open") { attachments.open(attachment) }
        .accessibilityAction(named: "Show in Finder") { attachments.reveal(attachment) }
        .accessibilityAction(named: "Remove") { Task { await attachments.remove(attachment) } }
    }
}

/// The icon the Finder shows for a file.
private struct FileIcon: View {
    let url: URL?
    let type: UTType

    var body: some View {
        Image(nsImage: url.map { NSWorkspace.shared.icon(forFile: $0.path(percentEncoded: false)) } ?? NSWorkspace.shared.icon(for: type))
            .resizable()
            .aspectRatio(contentMode: .fit)
    }
}

/// An attachment shown where the body says `![[name]]`: an image whole, a PDF as its first page
/// with Open, any other file as its icon and name.
struct AttachmentEmbed: View {
    let name: String
    let attachments: NoteAttachmentsModel?

    var body: some View {
        if let attachments, let attachment = attachments.attachment(named: name) {
            let url = attachments.file(of: name)
            if attachment.isImage, let url {
                EmbeddedImage(url: url, name: name)
            } else if attachment.isPDF, let url {
                EmbeddedPDF(attachments: attachments, attachment: attachment, url: url)
            } else {
                EmbeddedFile(attachments: attachments, attachment: attachment)
            }
        } else {
            Label(name, systemImage: "paperclip")
                .foregroundStyle(.secondary)
                .help(attachments?.isLoaded == false ? "Loading" : "This note has no attachment called \(name)")
        }
    }
}

/// An attached image, at most as wide as the text.
private struct EmbeddedImage: View {
    let url: URL
    let name: String
    @State private var image: NSImage?

    var body: some View {
        Group {
            if let image {
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(maxWidth: min(image.size.width, 720), alignment: .leading)
                    .clipShape(RoundedRectangle(cornerRadius: 6))
            } else {
                RoundedRectangle(cornerRadius: 6)
                    .fill(.quaternary.opacity(0.5))
                    .frame(height: 120)
            }
        }
        .accessibilityLabel(Text(name))
        .contextMenu {
            Button("Open", systemImage: "arrow.up.forward.app") { NSWorkspace.shared.open(url) }
            Button("Show in Finder", systemImage: "folder") { NSWorkspace.shared.activateFileViewerSelecting([url]) }
        }
        .task(id: url) {
            image = await Task.detached(priority: .userInitiated) { NSImage(contentsOf: url) }.value
        }
    }
}

/// An attached PDF: its first page, with its name, its number of pages and Open.
private struct EmbeddedPDF: View {
    let attachments: NoteAttachmentsModel
    let attachment: AttachmentInfo
    let url: URL
    @State private var page: NSImage?
    @State private var pages = 0

    var body: some View {
        HStack(alignment: .top, spacing: 14) {
            Group {
                if let page {
                    Image(nsImage: page)
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                } else {
                    Rectangle().fill(.quaternary.opacity(0.5))
                }
            }
            .frame(width: 150, height: 194)
            .background(.white)
            .clipShape(RoundedRectangle(cornerRadius: 4))
            .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(.separator))
            .onTapGesture(count: 2) { attachments.open(attachment) }
            VStack(alignment: .leading, spacing: 6) {
                Text(attachment.name).font(.headline).lineLimit(2)
                Text(pages == 1 ? "PDF, 1 page" : pages > 1 ? "PDF, \(pages) pages" : "PDF")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button("Open", systemImage: "arrow.up.forward.app") { attachments.open(attachment) }
                    .help("Open \(attachment.name) in the app the Mac uses for PDFs")
            }
        }
        .padding(10)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .combine)
        .task(id: url) {
            let (image, count) = await Task.detached(priority: .userInitiated) { () -> (NSImage?, Int) in
                guard let document = PDFDocument(url: url) else { return (nil, 0) }
                let image = document.page(at: 0)?.thumbnail(of: CGSize(width: 300, height: 388), for: .cropBox)
                return (image, document.pageCount)
            }.value
            page = image
            pages = count
        }
    }
}

/// Any other attached file: its icon, name and size, and Open.
private struct EmbeddedFile: View {
    let attachments: NoteAttachmentsModel
    let attachment: AttachmentInfo

    var body: some View {
        HStack(spacing: 10) {
            FileIcon(url: attachments.file(of: attachment.name), type: attachment.contentType)
                .frame(width: 32, height: 32)
            VStack(alignment: .leading, spacing: 2) {
                Text(attachment.name).lineLimit(1)
                Text(attachment.size.formatted(.byteCount(style: .file)))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 12)
            Button("Open", systemImage: "arrow.up.forward.app") { attachments.open(attachment) }
        }
        .padding(10)
        .frame(maxWidth: 420)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .combine)
    }
}

/// The outline shown while files are dragged over a place that attaches them.
struct DropHighlight: View {
    var body: some View {
        RoundedRectangle(cornerRadius: 8)
            .strokeBorder(KnodeColor.indigo, style: StrokeStyle(lineWidth: 2, dash: [6, 4]))
            .background(KnodeColor.indigo.opacity(0.06), in: RoundedRectangle(cornerRadius: 8))
            .padding(4)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
    }
}
