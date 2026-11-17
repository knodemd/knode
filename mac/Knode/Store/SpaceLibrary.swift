import Foundation

/// The spaces shown in the sidebar, like libraries in Apple Music: every space the person created
/// or opened, in the order they were added, until they remove it from the list. Removing a space
/// never touches its files.
///
/// Kept in the user defaults, each space as a bookmark (found again after a move or a rename of
/// the package), its path, and the name and icon it had when last seen, so the list shows without
/// opening every space.
struct SpaceLibrary {
    struct Entry: Hashable, Sendable {
        var url: URL
        var name: String
        var icon: String?
        /// True when the package is not where it was, such as on a disk that is not connected.
        var isMissing = false
    }

    private let defaults: UserDefaults
    private let key: String

    init(defaults: UserDefaults = .standard, key: String = "SpaceLibrary") {
        self.defaults = defaults
        self.key = key
    }

    /// `<name>-archive-<digits>.knode`: a copy onboarding put aside (`knode onboard archive`).
    static func isArchive(_ url: URL) -> Bool {
        let stem = url.deletingPathExtension().lastPathComponent
        guard let range = stem.range(of: "-archive-", options: .backwards) else { return false }
        let stamp = stem[range.upperBound...]
        return !stamp.isEmpty && stamp.allSatisfy { $0.isASCII && $0.isNumber }
    }

    /// False until the list is first saved: the app then fills it from the spaces it knows.
    var hasBeenSaved: Bool {
        defaults.object(forKey: key) != nil
    }

    var entries: [Entry] {
        let stored = defaults.array(forKey: key) as? [[String: Any]] ?? []
        return stored.compactMap { item in
            guard let path = item["path"] as? String else { return nil }
            var url = URL(filePath: path, directoryHint: .isDirectory).standardizedFileURL
            if let bookmark = item["bookmark"] as? Data {
                var isStale = false
                if let resolved = try? URL(resolvingBookmarkData: bookmark, options: [.withoutUI, .withoutMounting], bookmarkDataIsStale: &isStale) {
                    url = resolved.standardizedFileURL
                }
            }
            // A space archived from the command line moved; its bookmark followed it to the
            // archive. Archives are kept for recovery, not listed: drop it from the sidebar.
            if Self.isArchive(url) && !Self.isArchive(URL(filePath: path)) { return nil }
            let exists = FileManager.default.fileExists(atPath: url.path(percentEncoded: false))
            let name = item["name"] as? String ?? url.deletingPathExtension().lastPathComponent
            return Entry(url: url, name: name, icon: item["icon"] as? String, isMissing: !exists)
        }
    }

    /// Adds a space at the end of the list, or updates its name and icon if it is there already.
    func add(_ url: URL, name: String? = nil, icon: String?? = .none) {
        let url = url.standardizedFileURL
        var entries = self.entries
        if let index = entries.firstIndex(where: { $0.url == url }) {
            if let name { entries[index].name = name }
            if case .some(let icon) = icon { entries[index].icon = icon }
        } else {
            let name = name ?? SpaceManifest.name(at: url) ?? url.deletingPathExtension().lastPathComponent
            entries.append(Entry(url: url, name: name, icon: icon ?? nil))
        }
        save(entries)
        // Opening or creating a space the person took off the list puts it back for good.
        let path = url.path(percentEncoded: false)
        if removed.contains(path) {
            defaults.set(Array(removed.subtracting([path])).sorted(), forKey: removedKey)
        }
    }

    /// Takes a space off the list. Its files stay where they are, and it is not added back by
    /// `addKnown` until the person opens or creates it again.
    func remove(_ url: URL) {
        let url = url.standardizedFileURL
        let path = url.path(percentEncoded: false)
        save(entries.filter { $0.url.standardizedFileURL.path(percentEncoded: false) != path })
        defaults.set(Array(removed.union([path])).sorted(), forKey: removedKey)
    }

    /// Paths of the spaces the person took off the list.
    var removed: Set<String> {
        Set(defaults.stringArray(forKey: removedKey) ?? [])
    }

    /// Adds the spaces Knode knows that the list does not have yet, such as one made from the
    /// command line, by an agent or by an import, unless the person took it off the list.
    func addKnown(_ urls: [URL]) {
        let removed = self.removed
        var listed = Set(entries.map { $0.url.standardizedFileURL.path(percentEncoded: false) })
        for url in urls {
            let path = url.standardizedFileURL.path(percentEncoded: false)
            guard !listed.contains(path), !removed.contains(path), FileManager.default.fileExists(atPath: path) else { continue }
            add(url)
            listed.insert(path)
        }
    }

    private var removedKey: String { key + "Removed" }

    /// Saves the list as it is, empty or not, so it is no longer filled automatically.
    func markSaved() {
        save(entries)
    }

    private func save(_ entries: [Entry]) {
        defaults.set(entries.map { entry -> [String: Any] in
            var item: [String: Any] = ["path": entry.url.path(percentEncoded: false), "name": entry.name]
            if let bookmark = try? entry.url.bookmarkData(options: []) { item["bookmark"] = bookmark }
            if let icon = entry.icon { item["icon"] = icon }
            return item
        }, forKey: key)
    }
}
