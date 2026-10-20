import AppKit
import CoreText
import KnodeBrand
import SwiftUI

/// The four typefaces of the app. Never the system font.
/// - Knode Block: display titles (space name in the header, empty states, welcome).
/// - Knode Line: section labels, kickers, chips and key hints, uppercase with +2% tracking.
/// - Geist: all other text, note titles (semibold) and the preview.
/// - Geist Mono: the Markdown source, ids, paths and code.
enum KFont {
    static let geistName = "Geist-Regular"
    static let geistMonoName = "GeistMono-Regular"

    enum Weight: CGFloat, CaseIterable, Sendable {
        case regular = 400
        case medium = 500
        case semibold = 600
        case bold = 700
    }

    /// Registers Geist, Geist Mono, Knode Block and Knode Line for this process. Safe to call many
    /// times; true when all four are available.
    @discardableResult
    static func register() -> Bool { registered }

    private static let registered: Bool = {
        let bundle = Bundle(for: BundleToken.self)
        for file in ["Geist", "GeistMono"] {
            guard let url = bundle.url(forResource: file, withExtension: "ttf") else { return false }
            CTFontManagerRegisterFontsForURL(url as CFURL, .process, nil)
        }
        return KnodeFonts.register() && isAvailable(geistName) && isAvailable(geistMonoName)
    }()

    static func isAvailable(_ postScriptName: String) -> Bool {
        let font = CTFontCreateWithName(postScriptName as CFString, 12, nil)
        return CTFontCopyPostScriptName(font) as String == postScriptName
    }

    // MARK: AppKit

    /// Geist at a weight of its variable axis.
    static func nsUI(_ size: CGFloat, _ weight: Weight = .regular) -> NSFont {
        variable(geistName, size: size, weight: weight)
    }

    /// Geist Mono at a weight of its variable axis.
    static func nsMono(_ size: CGFloat, _ weight: Weight = .regular) -> NSFont {
        variable(geistMonoName, size: size, weight: weight)
    }

    private static func variable(_ name: String, size: CGFloat, weight: Weight) -> NSFont {
        register()
        guard let base = NSFont(name: name, size: size) else { return NSFont.systemFont(ofSize: size) }
        guard weight != .regular else { return base }
        let wght = NSNumber(value: 0x7767_6874) // 'wght'
        let descriptor = base.fontDescriptor.addingAttributes([.variation: [wght: NSNumber(value: Double(weight.rawValue))]])
        return NSFont(descriptor: descriptor, size: size) ?? base
    }

    // MARK: SwiftUI

    static func ui(_ size: CGFloat, _ weight: Weight = .regular) -> Font { Font(nsUI(size, weight) as CTFont) }
    static func mono(_ size: CGFloat, _ weight: Weight = .regular) -> Font { Font(nsMono(size, weight) as CTFont) }
    static func block(_ size: CGFloat) -> Font { .knodeBlock(size) }
    static func line(_ size: CGFloat) -> Font { .knodeLine(size) }
}

private final class BundleToken {}

extension View {
    /// Knode Line in its label form: uppercase, tracked out by 2%.
    func kLineStyle(_ size: CGFloat = 11) -> some View {
        font(KFont.line(size)).textCase(.uppercase).tracking(size * 0.02)
    }

    /// Knode Block display type: uppercase, as the typeface is drawn.
    func kBlockStyle(_ size: CGFloat) -> some View {
        font(KFont.block(size)).textCase(.uppercase)
    }
}
