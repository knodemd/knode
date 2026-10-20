import Foundation

/// Reads `manifest.json` of a `.knode` package without opening its database, so a list of spaces
/// can show their names (format specification, section 2).
enum SpaceManifest {
    private struct Contents: Decodable {
        var id: String
        var name: String
    }

    /// The name of the space at `url`, or `nil` when it has no readable manifest.
    static func name(at url: URL) -> String? {
        contents(at: url)?.name
    }

    /// The id of the space at `url`, or `nil` when it has no readable manifest.
    static func id(at url: URL) -> String? {
        contents(at: url)?.id
    }

    private static func contents(at url: URL) -> Contents? {
        guard let data = try? Data(contentsOf: url.appending(path: "manifest.json")) else { return nil }
        return try? JSONDecoder().decode(Contents.self, from: data)
    }
}
