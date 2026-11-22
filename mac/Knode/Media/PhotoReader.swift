import CoreGraphics
import Foundation
import ImageIO
import Vision

/// Why a photo could not be read.
enum PhotoReadingError: Error, LocalizedError, Equatable {
    case unreadableImage(String)

    var errorDescription: String? {
        switch self {
        case .unreadableImage(let name): "\(name) could not be read as an image."
        }
    }
}

/// Reads a photo on this Mac: what its file says (date, place, camera, size), the text in it and
/// what it shows.
protocol PhotoReader: Sendable {
    func read(_ url: URL) async throws -> PhotoReadingDraft
}

/// What a photo's file says about it, read with ImageIO: EXIF date, GPS, camera and size.
enum PhotoMetadataReader {
    static func read(_ url: URL) -> PhotoMetadataDraft? {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
        else { return nil }
        return metadata(properties)
    }

    static func metadata(_ properties: [CFString: Any]) -> PhotoMetadataDraft {
        var metadata = PhotoMetadataDraft()
        let exif = properties[kCGImagePropertyExifDictionary] as? [CFString: Any] ?? [:]
        let tiff = properties[kCGImagePropertyTIFFDictionary] as? [CFString: Any] ?? [:]
        let gps = properties[kCGImagePropertyGPSDictionary] as? [CFString: Any] ?? [:]
        if let original = exif[kCGImagePropertyExifDateTimeOriginal] as? String ?? tiff[kCGImagePropertyTIFFDateTime] as? String {
            metadata.takenAt = date(original, offset: exif[kCGImagePropertyExifOffsetTimeOriginal] as? String)
        }
        if let latitude = gps[kCGImagePropertyGPSLatitude] as? Double, let longitude = gps[kCGImagePropertyGPSLongitude] as? Double {
            let south = (gps[kCGImagePropertyGPSLatitudeRef] as? String)?.uppercased() == "S"
            let west = (gps[kCGImagePropertyGPSLongitudeRef] as? String)?.uppercased() == "W"
            let lat = south ? -latitude : latitude
            let lon = west ? -longitude : longitude
            // A bad GPS reading is left out, not sent.
            if (-90...90).contains(lat), (-180...180).contains(lon) {
                metadata.latitude = lat
                metadata.longitude = lon
            }
        }
        let make = (tiff[kCGImagePropertyTIFFMake] as? String)?.trimmingCharacters(in: .whitespaces) ?? ""
        let model = (tiff[kCGImagePropertyTIFFModel] as? String)?.trimmingCharacters(in: .whitespaces) ?? ""
        let camera = model.hasPrefix(make) ? model : [make, model].filter { !$0.isEmpty }.joined(separator: " ")
        if !camera.isEmpty { metadata.camera = camera }
        let width = properties[kCGImagePropertyPixelWidth] as? Int
        let height = properties[kCGImagePropertyPixelHeight] as? Int
        // The size as the photo is seen: EXIF orientations 5 to 8 turn it a quarter.
        let orientation = (properties[kCGImagePropertyOrientation] as? NSNumber)?.intValue ?? 1
        let turned = (5...8).contains(orientation)
        metadata.width = turned ? height : width
        metadata.height = turned ? width : height
        return metadata
    }

    /// An EXIF date, `yyyy:MM:dd HH:mm:ss`, in its offset (`+01:00`) when it has one, else in this
    /// Mac's time zone, as the camera's clock most likely was.
    static func date(_ text: String, offset: String?, timeZone: TimeZone = .current) -> Date? {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy:MM:dd HH:mm:ss"
        formatter.timeZone = offset.flatMap(Self.timeZone(offset:)) ?? timeZone
        return formatter.date(from: text.trimmingCharacters(in: .whitespaces))
    }

    private static func timeZone(offset: String) -> TimeZone? {
        let match = offset.wholeMatch(of: /([+-])(\d\d):(\d\d)/)
        guard let match, let hours = Int(match.2), let minutes = Int(match.3) else { return nil }
        let seconds = (hours * 3600 + minutes * 60) * (match.1 == "-" ? -1 : 1)
        return TimeZone(secondsFromGMT: seconds)
    }
}

/// Reads photos with Vision on this Mac: the text in them (`VNRecognizeTextRequest`, accurate,
/// the language found by itself, Spanish included) and labels for what they show
/// (`VNClassifyImageRequest`, the most likely ones above a confidence). Nothing leaves the Mac.
/// The place is left out: naming it from coordinates needs the network.
struct VisionPhotoReader: PhotoReader {
    /// The least confidence for a label.
    var minimumConfidence: Float = 0.3
    /// The most labels kept.
    var maximumLabels = 8
    /// The longest side of the image Vision reads, in pixels.
    static let maxPixelSize = 4096

    func read(_ url: URL) async throws -> PhotoReadingDraft {
        let minimumConfidence = minimumConfidence
        let maximumLabels = maximumLabels
        return try await Task.detached(priority: .utility) {
            try Self.readNow(url, minimumConfidence: minimumConfidence, maximumLabels: maximumLabels)
        }.value
    }

    private static func readNow(_ url: URL, minimumConfidence: Float, maximumLabels: Int) throws -> PhotoReadingDraft {
        // A thumbnail of at most `maxPixelSize`, already turned upright: enough for Vision, and it
        // decodes a HEIC (or a huge image) without holding it whole in memory.
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixelSize,
        ]
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
              let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
        else { throw PhotoReadingError.unreadableImage(url.lastPathComponent) }
        let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any] ?? [:]

        let text = VNRecognizeTextRequest()
        text.recognitionLevel = .accurate
        text.usesLanguageCorrection = true
        text.automaticallyDetectsLanguage = true
        let classify = VNClassifyImageRequest()
        let handler = VNImageRequestHandler(cgImage: image, orientation: .up)
        try handler.perform([text, classify])

        let lines = (text.results ?? []).compactMap { $0.topCandidates(1).first?.string }
        let labels = (classify.results ?? [])
            .filter { $0.confidence >= minimumConfidence }
            .sorted { $0.confidence > $1.confidence }
            .prefix(maximumLabels)
            .map { $0.identifier.replacingOccurrences(of: "_", with: " ") }
        return PhotoReadingDraft(
            text: lines.joined(separator: "\n"), labels: Array(labels), description: nil,
            metadata: PhotoMetadataReader.metadata(properties)
        )
    }
}
