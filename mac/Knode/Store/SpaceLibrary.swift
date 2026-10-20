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
    }

    /// Takes a space off the list. Its files stay where they are.
    func remove(_ url: URL) {
        let url = url.standardizedFileURL
        save(entries.filter { $0.url != url })
    }

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
