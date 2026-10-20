import AppKit
import KnodeBrand
import SwiftUI

/// The colours of knode.md (web/src/index.css), as 24-bit RGB values, for one appearance.
struct KPalette: Equatable, Sendable {
    var bg: UInt32
    var panel: UInt32
    var line: UInt32
    var fg: UInt32
    var muted: UInt32
    var accent: UInt32
    var onAccent: UInt32

    /// Ink: the default when the system is dark.
    static let dark = KPalette(bg: 0x0B0B14, panel: 0x11111C, line: 0x24243B, fg: 0xF3F2EE,
                               muted: 0xA3A2BA, accent: 0x818CF8, onAccent: 0x0B0B14)
    /// Paper. Indigo is never small text on paper, so the accent is ink.
    static let light = KPalette(bg: 0xF3F2EE, panel: 0xFFFFFF, line: 0xDAD8E2, fg: 0x0B0B14,
                                muted: 0x5E5D73, accent: 0x0B0B14, onAccent: 0xF3F2EE)

    /// Indigo, the same in both appearances: highlights, links, selection marks, the caret.
    static let indigo: UInt32 = 0x818CF8
}

/// The colour tokens of the app for one appearance. Views read it from the environment
/// (`@Environment(\.kTheme)`), which `knodeTheme()` keeps in step with the colour scheme.
struct KTheme: Equatable, Sendable {
    let scheme: ColorScheme
    let palette: KPalette

    var isDark: Bool { scheme == .dark }

    var bg: Color { Color(hex: palette.bg) }
    var panel: Color { Color(hex: palette.panel) }
    var line: Color { Color(hex: palette.line) }
    var fg: Color { Color(hex: palette.fg) }
    var muted: Color { Color(hex: palette.muted) }
    var accent: Color { Color(hex: palette.accent) }
    var onAccent: Color { Color(hex: palette.onAccent) }
    var indigo: Color { Color(hex: KPalette.indigo) }

    /// Text under the pointer: indigo on ink; on paper it stays ink and gets an indigo underline.
    var hover: Color { isDark ? indigo : fg }
    var hoverUnderline: Bool { !isDark }
    /// The fill of a selected row or a pressed control, under the indigo marker.
    var highlight: Color { isDark ? indigo.opacity(0.12) : indigo.opacity(0.14) }
    /// The fill of a row under the pointer.
    var hoverFill: Color { fg.opacity(isDark ? 0.05 : 0.04) }
    /// The fill of text fields, like the web's `--field-opacity` over the line colour.
    var field: Color { isDark ? Color(hex: 0x16162A) : Color(hex: 0xFFFFFF) }
    /// Text selection in editors and fields.
    var selection: Color { indigo.opacity(isDark ? 0.32 : 0.30) }
    /// Destructive actions and errors only: the one colour outside the three inks, kept muted.
    var danger: Color { isDark ? Color(hex: 0xF87171) : Color(hex: 0xB42318) }
    var dangerFill: Color { danger.opacity(isDark ? 0.14 : 0.10) }

    static let dark = KTheme(scheme: .dark, palette: .dark)
    static let light = KTheme(scheme: .light, palette: .light)

    static func resolved(for scheme: ColorScheme) -> KTheme { scheme == .dark ? .dark : .light }
}

/// Corner radii: cards 18, rows 10, buttons and fields 12, chips fully round.
enum KRadius {
    static let card: CGFloat = 18
    static let control: CGFloat = 12
    static let row: CGFloat = 10
    static let small: CGFloat = 8
    static let chip: CGFloat = 999
}

/// Spacing on a 4 pt grid.
enum KSpace {
    static let x1: CGFloat = 4
    static let x2: CGFloat = 8
    static let x3: CGFloat = 12
    static let x4: CGFloat = 16
    static let x5: CGFloat = 20
    static let x6: CGFloat = 24
    static let x8: CGFloat = 32
    static let x10: CGFloat = 40
}

/// Motion: short and eased out, and none at all with Reduce Motion.
enum KMotion {
    static let fast: Double = 0.12
    static let standard: Double = 0.18

    static func animation(_ duration: Double = standard, reduceMotion: Bool) -> Animation? {
        reduceMotion ? nil : .easeOut(duration: duration)
    }
}

/// Appearance-aware colours for AppKit views and places without the SwiftUI environment.
enum KColor {
    static func ns(_ token: KeyPath<KPalette, UInt32>) -> NSColor {
        NSColor(name: nil) { appearance in
            let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            return NSColor(hex: (dark ? KPalette.dark : KPalette.light)[keyPath: token])
        }
    }

    static var indigo: NSColor { NSColor(hex: KPalette.indigo) }
    static var bg: Color { Color(nsColor: ns(\.bg)) }
    static var panel: Color { Color(nsColor: ns(\.panel)) }
    static var line: Color { Color(nsColor: ns(\.line)) }
    static var fg: Color { Color(nsColor: ns(\.fg)) }
    static var muted: Color { Color(nsColor: ns(\.muted)) }
    static var accent: Color { Color(nsColor: ns(\.accent)) }
}

extension Color {
    init(hex: UInt32, opacity: Double = 1) {
        self.init(.sRGB, red: Double((hex >> 16) & 0xFF) / 255, green: Double((hex >> 8) & 0xFF) / 255,
                  blue: Double(hex & 0xFF) / 255, opacity: opacity)
    }
}

extension NSColor {
    convenience init(hex: UInt32, alpha: CGFloat = 1) {
        self.init(srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
                  blue: CGFloat(hex & 0xFF) / 255, alpha: alpha)
    }
}

extension EnvironmentValues {
    @Entry var kTheme: KTheme = .dark
}

/// Puts the theme matching the colour scheme in the environment, with Geist as the text font,
/// the Knode colours as foreground and tint, and the background colour behind.
struct KThemeModifier: ViewModifier {
    @Environment(\.colorScheme) private var scheme
    var background: Bool

    func body(content: Content) -> some View {
        let theme = KTheme.resolved(for: scheme)
        content
            .environment(\.kTheme, theme)
            .font(KFont.ui(13))
            .foregroundStyle(theme.fg)
            .tint(theme.isDark ? theme.indigo : theme.accent)
            .background { if background { theme.bg.ignoresSafeArea() } }
    }
}

extension View {
    /// The Knode theme for this view and everything in it. Apply it at the root of every window,
    /// sheet and floating panel.
    func knodeTheme(background: Bool = false) -> some View {
        modifier(KThemeModifier(background: background))
    }
}
