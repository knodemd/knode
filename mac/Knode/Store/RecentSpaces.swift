import Foundation

/// The spaces opened most recently, newest first, kept in the user defaults as bookmarks so they
/// are found again after being moved or renamed.
///
/// The app is not sandboxed in v0.1, so the bookmarks carry no security scope.
struct RecentSpaces {
    static let limit = 10

    private let defaults: UserDefaults
    private let key: String

    init(defaults: UserDefaults = .standard, key: String = "RecentSpaces") {
        self.defaults = defaults
        self.key = key
    }

    /// The recent spaces that still exist, newest first.
    var urls: [URL] {
        let bookmarks = defaults.array(forKey: key) as? [Data] ?? []
        return bookmarks.compactMap { data in
            var isStale = false
            guard let url = try? URL(resolvingBookmarkData: data, options: [.withoutUI, .withoutMounting], bookmarkDataIsStale: &isStale),
                  FileManager.default.fileExists(atPath: url.path(percentEncoded: false))
            else { return nil }
            return url.standardizedFileURL
        }
    }

    /// Puts a space at the top of the list.
    func add(_ url: URL) {
        let url = url.standardizedFileURL
        let urls = [url] + self.urls.filter { $0 != url }
        save(Array(urls.prefix(Self.limit)))
    }

    func clear() {
        defaults.removeObject(forKey: key)
    }

    private func save(_ urls: [URL]) {
        defaults.set(urls.compactMap { try? $0.bookmarkData(options: []) }, forKey: key)
    }
}
