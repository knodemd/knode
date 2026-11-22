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

/// Web apps and other apps named after a service, for sources with no native app here: a
/// Safari web app (`com.apple.Safari.WebApp.*`) or a Chrome app shim (`com.google.Chrome.app.*`)
/// called Gmail, or an app called Slack, gives its icon to the source.
struct WebAppFinder: Sendable {
    /// What an app bundle says about itself.
    struct Bundle: Equatable, Sendable {
        var name: String?
        var bundleID: String?
    }

    /// The folders looked in, in order.
    var folders: [URL]
    /// The names of the items in a folder.
    var contents: @Sendable (URL) -> [String]
    /// What the app at a URL says about itself, from its `Info.plist`.
    var bundle: @Sendable (URL) -> Bundle?

    /// Source id to the names its apps go by.
    static let names: [String: [String]] = [
        "gmail": ["Gmail"],
        "slack": ["Slack"],
        "telegram": ["Telegram"],
        "whatsapp": ["WhatsApp"],
        "google-calendar": ["Google Calendar"],
        "discord": ["Discord"],
        "notion": ["Notion"],
    ]

    /// The names the apps of a source go by. Variants such as `whatsapp-mac` take those of the
    /// longest known id they start with.
    static func names(for sourceID: String) -> [String] {
        let id = sourceID.lowercased()
        if let exact = names[id] { return exact }
        let prefix = names.keys.filter { id.hasPrefix($0 + "-") }.max { $0.count < $1.count }
        return prefix.flatMap { names[$0] } ?? []
    }

    static func isWebApp(_ bundleID: String?) -> Bool {
        guard let bundleID else { return false }
        return bundleID.hasPrefix("com.apple.Safari.WebApp.") || bundleID.hasPrefix("com.google.Chrome.app.")
    }

    /// The first app named after the source, web apps first, in the order of `folders`.
    func appURL(for sourceID: String) -> URL? {
        let wanted = Set(Self.names(for: sourceID).map { $0.lowercased() })
        guard !wanted.isEmpty else { return nil }
        var matches: [(url: URL, isWebApp: Bool)] = []
        for folder in folders {
            for item in contents(folder) where item.lowercased().hasSuffix(".app") {
                let url = folder.appending(path: item, directoryHint: .isDirectory)
                let info = bundle(url)
                let fileName = String(item.dropLast(4))
                let named = [fileName, info?.name].compactMap { $0?.trimmingCharacters(in: .whitespaces).lowercased() }
                if named.contains(where: wanted.contains) {
                    matches.append((url, Self.isWebApp(info?.bundleID)))
                }
            }
        }
        return (matches.first { $0.isWebApp } ?? matches.first)?.url
    }

    /// `~/Applications`, its Chrome Apps folder, and `/Applications`.
    static var live: WebAppFinder {
        let home = FileManager.default.homeDirectoryForCurrentUser.appending(path: "Applications", directoryHint: .isDirectory)
        return WebAppFinder(
            folders: [home, home.appending(path: "Chrome Apps.localized", directoryHint: .isDirectory), URL(filePath: "/Applications", directoryHint: .isDirectory)],
            contents: { (try? FileManager.default.contentsOfDirectory(atPath: $0.path(percentEncoded: false))) ?? [] },
            bundle: { url in
                guard let info = NSDictionary(contentsOf: url.appending(path: "Contents/Info.plist")) else { return nil }
                return Bundle(name: (info["CFBundleDisplayName"] ?? info["CFBundleName"]) as? String, bundleID: info["CFBundleIdentifier"] as? String)
            }
        )
    }
}

/// The icon of an installed app, read once per source.
@MainActor
enum SourceIconCache {
    private static var icons: [String: NSImage?] = [:]

    static func icon(for sourceID: String) -> NSImage? {
        if let known = icons[sourceID] { return known }
        let url = SourceApps.appURL(for: sourceID) { NSWorkspace.shared.urlForApplication(withBundleIdentifier: $0) }
            ?? WebAppFinder.live.appURL(for: sourceID)
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
