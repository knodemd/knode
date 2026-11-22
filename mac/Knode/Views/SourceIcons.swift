import AppKit
import SwiftUI

/// The app each source reads, so a source shows with that app's own icon, read from the copy
/// installed on this Mac. No third-party logo ships with Knode.
enum SourceApps {
    /// Source id to the bundle ids of its app, the first installed one winning.
    static let bundleIDs: [String: [String]] = [
        "apple-mail": ["com.apple.mail"],
        "mail": ["com.apple.mail"],
        "messages": ["com.apple.MobileSMS"],
        "imessage": ["com.apple.MobileSMS"],
        "voice-memos": ["com.apple.VoiceMemos"],
        "calendar": ["com.apple.iCal"],
        "calendars": ["com.apple.iCal"],
        "photos": ["com.apple.Photos"],
        "whatsapp": ["net.whatsapp.WhatsApp", "desktop.WhatsApp"],
        "telegram": ["ru.keepcoder.Telegram", "org.telegram.desktop"],
        "slack": ["com.tinyspeck.slackmacgap"],
        "apple-notes": ["com.apple.Notes"],
        "notes": ["com.apple.Notes"],
        "bear": ["net.shinyfrog.bear"],
        "obsidian": ["md.obsidian"],
        "notion": ["notion.id"],
        "discord": ["com.hnc.Discord"],
    ]

    /// The bundle ids of the app a source reads. Variants such as `whatsapp-mac` or
    /// `photos-library` take the app of the longest known id they start with. Empty for sources
    /// with no app on the Mac, such as Gmail or IMAP.
    static func bundleIDs(for sourceID: String) -> [String] {
        let id = sourceID.lowercased()
        if let exact = bundleIDs[id] { return exact }
        let prefix = bundleIDs.keys.filter { id.hasPrefix($0 + "-") }.max { $0.count < $1.count }
        return prefix.flatMap { bundleIDs[$0] } ?? []
    }

    /// Where the app of a source is installed, through `locate` (Launch Services in the app).
    static func appURL(for sourceID: String, locate: (String) -> URL?) -> URL? {
        bundleIDs(for: sourceID).lazy.compactMap(locate).first
    }

    /// The Knode icon shown when the app is not installed, or the source has none.
    static func glyph(for sourceID: String) -> KGlyph {
        let id = sourceID.lowercased()
        func has(_ words: String...) -> Bool { words.contains { id.contains($0) } }
        if has("mail", "imap") { return .mail }
        if has("calendar") { return .calendar }
        if has("photo") { return .image }
        if has("voice", "memo", "recording") { return .people }
        if has("message", "whatsapp", "telegram", "slack", "discord", "chat") { return .chat }
        if has("note", "bear", "obsidian", "notion") { return .note }
        return .import
    }
}

/// The icon of an installed app, read once per source.
@MainActor
enum SourceIconCache {
    private static var icons: [String: NSImage?] = [:]

    static func icon(for sourceID: String) -> NSImage? {
        if let known = icons[sourceID] { return known }
        let url = SourceApps.appURL(for: sourceID) { NSWorkspace.shared.urlForApplication(withBundleIdentifier: $0) }
        let icon = url.map { NSWorkspace.shared.icon(forFile: $0.path(percentEncoded: false)) }
        icons[sourceID] = icon
        return icon
    }
}

/// A source's app icon, or its Knode icon in a tinted rounded square when the app is not here.
struct SourceIcon: View {
    let sourceID: String
    var size: CGFloat = 36
    @Environment(\.kTheme) private var theme
    @State private var image: NSImage?

    var body: some View {
        Group {
            if let image {
                Image(nsImage: image)
                    .resizable()
                    .interpolation(.high)
                    .aspectRatio(contentMode: .fit)
            } else {
                RoundedRectangle(cornerRadius: size * 0.24, style: .continuous)
                    .fill(theme.indigo.opacity(theme.isDark ? 0.22 : 0.16))
                    .overlay {
                        KIcon(SourceApps.glyph(for: sourceID), size: size >= 32 ? .md : .sm,
                              color: theme.isDark ? theme.indigo : theme.fg)
                    }
                    .padding(size * 0.06)
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
        .task(id: sourceID) { image = SourceIconCache.icon(for: sourceID) }
    }
}
