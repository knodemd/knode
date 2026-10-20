import AppKit
import PDFKit
import QuickLookThumbnailing
import SwiftUI
import UniformTypeIdentifiers

/// The files attached to the open note, beside it: open one, show it in the Finder or remove it,
/// and attach more by dropping files on the panel or choosing them.
struct NoteAttachmentsPanel: View {
    @Bindable var attachments: NoteAttachmentsModel
    @Environment(AppModel.self) private var model
    @Environment(\.kTheme) private var theme
    @State private var isChoosing = false
    @State private var isTargeted = false

    private let columns = [GridItem(.flexible(), spacing: KSpace.x2), GridItem(.flexible(), spacing: KSpace.x2)]

    var body: some View {
        VStack(spacing: 0) {
            KScrollView {
                VStack(alignment: .leading, spacing: KSpace.x3) {
                    KSectionLabel("Attachments") {
                        if !attachments.attachments.isEmpty {
                            Text("\(attachments.attachments.count)").kLineStyle(10).foregroundStyle(theme.muted).monospacedDigit()
                        }
                    }
                    if attachments.isLoaded, attachments.attachments.isEmpty {
                        VStack(spacing: KSpace.x3) {
                            KIcon(.attach, size: .lg, color: theme.muted)
                            Text("Drop files on the note, or paste an image, to attach them.")
                                .font(KFont.ui(12))
                                .foregroundStyle(theme.muted)
                                .multilineTextAlignment(.center)
                        }
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, KSpace.x8)
                        .padding(.horizontal, KSpace.x4)
                        .overlay(RoundedRectangle(cornerRadius: KRadius.card, style: .continuous)
                            .strokeBorder(theme.line, style: StrokeStyle(lineWidth: 1, dash: [4, 4])))
                    } else if !attachments.isLoaded {
                        KSpinner().frame(maxWidth: .infinity).padding(KSpace.x6)
                    }
                    LazyVGrid(columns: columns, spacing: KSpace.x2) {
                        ForEach(attachments.attachments) { attachment in
                            AttachmentCard(attachments: attachments, attachment: attachment)
                        }
                    }
                }
                .padding(KSpace.x4)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            InspectorFooter(error: attachments.errorMessage) {
                Button { isChoosing = true } label: {
                    HStack(spacing: KSpace.x2) {
                        if attachments.isAttaching { KSpinner(size: 12) } else { KIcon(.attach, size: .sm) }
                        Text("Attach files")
                    }
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(KButtonStyle(kind: .secondary))
                .disabled(attachments.isAttaching)
                .kTooltip("Attach files to this note; each shows where the note ends")
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(theme.panel)
        .overlay {
            if isTargeted { DropHighlight() }
        }
        .onDrop(of: [.fileURL], isTargeted: $isTargeted) { providers in
            Task { await model.attachFilesAtEnd(await DroppedItems.fileURLs(providers)) }
            return true
        }
        .fileImporter(isPresented: $isChoosing, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            guard case .success(let urls) = result else { return }
            Task { await model.attachFilesAtEnd(urls) }
        }
        .task(id: attachments.nodeID) { await attachments.load() }
        .knodeTheme()
    }
}

/// One attached file in the inspector: its thumbnail in a card, its name and size.
private struct AttachmentCard: View {
    let attachments: NoteAttachmentsModel
    let attachment: AttachmentInfo
    @Environment(\.kTheme) private var theme
    @State private var hovering = false

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: 14, style: .continuous)
        VStack(alignment: .leading, spacing: 0) {
            FileThumbnail(url: attachments.file(of: attachment.name), attachment: attachment)
                .frame(maxWidth: .infinity)
                .frame(height: 92)
                .clipped()
                .overlay(alignment: .bottom) { Rectangle().fill(theme.line).frame(height: 1) }
            VStack(alignment: .leading, spacing: 3) {
                Text(attachment.name)
                    .font(KFont.ui(12, .medium))
                    .foregroundStyle(hovering ? theme.hover : theme.fg)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(attachment.size.formatted(.byteCount(style: .file)))
                    .kLineStyle(9.5)
                    .foregroundStyle(theme.muted)
                    .lineLimit(1)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
        }
        .background(shape.fill(theme.bg.opacity(theme.isDark ? 0.6 : 0.5)))
        .clipShape(shape)
        .overlay(shape.strokeBorder(hovering ? theme.muted.opacity(0.6) : theme.line))
        .contentShape(shape)
        .onHover { hovering = $0 }
        .onTapGesture(count: 2) { attachments.open(attachment) }
        .kTooltip("\(attachment.name), attached by \(attachment.author.name) \(attachment.createdAt.formatted(date: .abbreviated, time: .shortened))")
        .kContextMenu {
            [
                .action("Open", icon: .expand) { attachments.open(attachment) },
                .action("Show in Finder", icon: .folder) { attachments.reveal(attachment) },
                .action("Copy embed", icon: .copy) {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(attachment.embed, forType: .string)
                },
                .divider,
                .action("Remove attachment", icon: .delete, isDestructive: true) {
                    Task { await attachments.remove(attachment) }
                },
            ]
        }
        .accessibilityElement(children: .combine)
        .accessibilityAction(named: "Open") { attachments.open(attachment) }
        .accessibilityAction(named: "Show in Finder") { attachments.reveal(attachment) }
        .accessibilityAction(named: "Remove") { Task { await attachments.remove(attachment) } }
    }
}

/// A preview of a file: its Quick Look thumbnail when it has one, otherwise a Knode icon with
/// its extension.
private struct FileThumbnail: View {
    let url: URL?
    let attachment: AttachmentInfo
    @Environment(\.kTheme) private var theme
    @Environment(\.displayScale) private var scale
    @State private var image: NSImage?

    var body: some View {
        ZStack {
            theme.field
            if let image {
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: attachment.isImage ? .fill : .fit)
                    .padding(attachment.isImage ? 0 : KSpace.x2)
            } else {
                VStack(spacing: 6) {
                    KIcon(FileGlyph.of(attachment.contentType), size: .lg, color: theme.muted)
                    Text(FileGlyph.extensionLabel(attachment.name)).kLineStyle(9.5).foregroundStyle(theme.muted)
                }
            }
        }
        .task(id: url) {
            guard let url else { return }
            let request = QLThumbnailGenerator.Request(fileAt: url, size: CGSize(width: 280, height: 184), scale: scale, representationTypes: .thumbnail)
            image = try? await QLThumbnailGenerator.shared.generateBestRepresentation(for: request).nsImage
        }
    }
}

/// The Knode icon of a kind of file.
enum FileGlyph {
    static func of(_ type: UTType) -> KGlyph {
        if type.conforms(to: .image) { return .image }
        if type.conforms(to: .sourceCode) || type.conforms(to: .script) { return .code }
        if type.conforms(to: .text) { return .text }
        if type.conforms(to: .archive) { return .archive }
        if type.conforms(to: .folder) { return .folder }
        return .file
    }

    static func extensionLabel(_ name: String) -> String {
        let ext = (name as NSString).pathExtension
        return ext.isEmpty ? "File" : ext
    }
}

/// An attachment shown where the body says `![[name]]`: an image whole, a PDF as its first page
/// with Open, any other file as its icon and name.
struct AttachmentEmbed: View {
    let name: String
    let attachments: NoteAttachmentsModel?
    @Environment(\.kTheme) private var theme

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
            HStack(spacing: 6) {
                KIcon(.attach, size: .sm)
                Text(name).font(KFont.ui(13))
            }
            .foregroundStyle(theme.muted)
            .kTooltip(attachments?.isLoaded == false ? "Loading" : "This note has no attachment called \(name)")
        }
    }
}

/// An attached image, at most as wide as the text.
private struct EmbeddedImage: View {
    let url: URL
    let name: String
    @Environment(\.kTheme) private var theme
    @State private var image: NSImage?

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: KRadius.control, style: .continuous)
        Group {
            if let image {
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(maxWidth: min(image.size.width, 720), alignment: .leading)
                    .clipShape(shape)
                    .overlay(shape.strokeBorder(theme.line))
            } else {
                shape.fill(theme.field)
                    .overlay(shape.strokeBorder(theme.line))
                    .overlay { KIcon(.image, size: .lg, color: theme.muted) }
                    .frame(height: 120)
            }
        }
        .accessibilityLabel(Text(name))
        .kContextMenu {
            [
                .action("Open", icon: .expand) { NSWorkspace.shared.open(url) },
                .action("Show in Finder", icon: .folder) { NSWorkspace.shared.activateFileViewerSelecting([url]) },
            ]
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
    @Environment(\.kTheme) private var theme
    @State private var page: NSImage?
    @State private var pages = 0

    var body: some View {
        let pageShape = RoundedRectangle(cornerRadius: KRadius.small, style: .continuous)
        HStack(alignment: .top, spacing: KSpace.x4) {
            Group {
                if let page {
                    Image(nsImage: page)
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                } else {
                    theme.field.overlay { KIcon(.file, size: .lg, color: theme.muted) }
                }
            }
            .frame(width: 150, height: 194)
            .background(Color(hex: KPalette.light.panel))
            .clipShape(pageShape)
            .overlay(pageShape.strokeBorder(theme.line))
            .onTapGesture(count: 2) { attachments.open(attachment) }
            VStack(alignment: .leading, spacing: KSpace.x2) {
                Text(attachment.name).font(KFont.ui(14, .semibold)).foregroundStyle(theme.fg).lineLimit(2)
                Text(pages == 1 ? "PDF, 1 page" : pages > 1 ? "PDF, \(pages) pages" : "PDF")
                    .kLineStyle(10)
                    .foregroundStyle(theme.muted)
                KButton("Open", icon: .expand, size: .small) { attachments.open(attachment) }
                    .kTooltip("Open \(attachment.name) in the app the Mac uses for PDFs")
                    .padding(.top, KSpace.x1)
            }
        }
        .padding(KSpace.x3)
        .background(RoundedRectangle(cornerRadius: KRadius.card, style: .continuous).fill(theme.panel))
        .overlay(RoundedRectangle(cornerRadius: KRadius.card, style: .continuous).strokeBorder(theme.line))
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
    @Environment(\.kTheme) private var theme

    var body: some View {
        HStack(spacing: KSpace.x3) {
            KIcon(FileGlyph.of(attachment.contentType), size: .lg, color: theme.isDark ? theme.indigo : theme.fg)
                .frame(width: 36, height: 36)
                .background(RoundedRectangle(cornerRadius: KRadius.row, style: .continuous).fill(theme.field))
            VStack(alignment: .leading, spacing: 3) {
                Text(attachment.name).font(KFont.ui(13, .medium)).foregroundStyle(theme.fg).lineLimit(1)
                Text("\(FileGlyph.extensionLabel(attachment.name))  \(attachment.size.formatted(.byteCount(style: .file)))")
                    .kLineStyle(10)
                    .foregroundStyle(theme.muted)
            }
            Spacer(minLength: KSpace.x3)
            KButton("Open", icon: .expand, size: .small) { attachments.open(attachment) }
        }
        .padding(KSpace.x3)
        .frame(maxWidth: 420)
        .background(RoundedRectangle(cornerRadius: KRadius.card, style: .continuous).fill(theme.panel))
        .overlay(RoundedRectangle(cornerRadius: KRadius.card, style: .continuous).strokeBorder(theme.line))
        .accessibilityElement(children: .combine)
    }
}

/// The outline shown while files are dragged over a place that attaches them.
struct DropHighlight: View {
    @Environment(\.kTheme) private var theme

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: KRadius.control, style: .continuous)
        shape
            .strokeBorder(theme.indigo, style: StrokeStyle(lineWidth: 1.5, dash: [6, 4]))
            .background(shape.fill(theme.indigo.opacity(0.06)))
            .padding(4)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
    }
}
