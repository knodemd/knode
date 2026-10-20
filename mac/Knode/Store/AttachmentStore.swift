import Foundation
import UniformTypeIdentifiers

/// The files attached to notes: images, PDFs and other files kept inside the space, in its
/// `blobs/` folder, stored once by the hash of their content.
///
/// `![[name]]` in a note's body shows an attachment of the note, and `[[name]]` links to it.
/// `ServiceSpaceStore` implements it through Knode Service, `CoreSpaceStore` with the core in the
/// app, and `RoutingSpaceStore` with whichever of the two holds the space.
public protocol AttachmentStore: Sendable {
    /// The files attached to a node, oldest first.
    func attachments(of node: Node.ID, in space: Space.ID) async throws -> [AttachmentInfo]

    /// Attaches the file at `url` to a node, under its file name, and returns the attachment.
    func attachFile(at url: URL, to node: Node.ID, in space: Space.ID) async throws -> AttachmentInfo

    /// Attaches bytes, such as a pasted image, to a node under `name`.
    func attach(_ data: Data, named name: String, to node: Node.ID, in space: Space.ID) async throws -> AttachmentInfo

    /// Removes an attachment from a node. The file stays in the space until unused files are
    /// purged, so the change can be undone.
    func removeAttachment(named name: String, from node: Node.ID, in space: Space.ID) async throws

    /// Where the file of an attachment is on this Mac, to show or open it, or nil when it is
    /// missing. Never written to.
    func attachmentURL(named name: String, of node: Node.ID, in space: Space.ID) async throws -> URL?
}

/// A file attached to a note.
public struct AttachmentInfo: Identifiable, Hashable, Sendable {
    public var nodeID: Node.ID
    /// Its name in the note, usually the original file name, such as `diagram.png`.
    public var name: String
    /// The SHA-256 of its content.
    public var hash: String
    public var size: Int64
    /// Such as `image/png` or `application/pdf`.
    public var mediaType: String
    public var createdAt: Date
    public var author: Author
    public var id: String { name }

    public init(nodeID: Node.ID, name: String, hash: String, size: Int64, mediaType: String, createdAt: Date, author: Author) {
        self.nodeID = nodeID
        self.name = name
        self.hash = hash
        self.size = size
        self.mediaType = mediaType
        self.createdAt = createdAt
        self.author = author
    }

    /// The type of the file, from its media type or else its name.
    public var contentType: UTType {
        UTType(mimeType: mediaType).flatMap { $0 == .data ? nil : $0 }
            ?? UTType(filenameExtension: (name as NSString).pathExtension)
            ?? .data
    }

    public var isImage: Bool { contentType.conforms(to: .image) }
    public var isPDF: Bool { contentType.conforms(to: .pdf) }

    /// What the body says to show the file where it is written.
    public var embed: String { Self.embed(name) }

    /// `![[name]]`: shows the attachment called `name` in the note.
    public static func embed(_ name: String) -> String { "![[\(name)]]" }

    /// The most bytes Knode attaches from one file.
    public static let maxSize: Int64 = 100 * 1024 * 1024
}
