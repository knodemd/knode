import AppKit
import KnodeBrand
import SwiftUI
import Testing
@testable import Knode

@MainActor
struct DesignSystemTests {
    @Test func themeTokensMatchTheWebsite() {
        #expect(KPalette.dark == KPalette(bg: 0x0B0B14, panel: 0x11111C, line: 0x24243B, fg: 0xF3F2EE,
                                          muted: 0xA3A2BA, accent: 0x818CF8, onAccent: 0x0B0B14))
        #expect(KPalette.light == KPalette(bg: 0xF3F2EE, panel: 0xFFFFFF, line: 0xDAD8E2, fg: 0x0B0B14,
                                           muted: 0x5E5D73, accent: 0x0B0B14, onAccent: 0xF3F2EE))
        #expect(KPalette.indigo == 0x818CF8)
    }

    @Test func themeFollowsTheColorScheme() {
        #expect(KTheme.resolved(for: .dark) == .dark)
        #expect(KTheme.resolved(for: .light) == .light)
        #expect(KTheme.dark.isDark && !KTheme.light.isDark)
        // On paper indigo is never text: the hover colour stays ink and gets an underline.
        #expect(KTheme.light.hover == KTheme.light.fg && KTheme.light.hoverUnderline)
        #expect(KTheme.dark.hover == KTheme.dark.indigo && !KTheme.dark.hoverUnderline)
    }

    @Test func appKitColorsResolveForEachAppearance() throws {
        let bg = KColor.ns(\.bg)
        func hex(_ color: NSColor, _ name: NSAppearance.Name) throws -> UInt32 {
            var resolved: NSColor?
            try #require(NSAppearance(named: name)).performAsCurrentDrawingAppearance {
                resolved = color.usingColorSpace(.sRGB)
            }
            let c = try #require(resolved)
            return UInt32((c.redComponent * 255).rounded()) << 16 | UInt32((c.greenComponent * 255).rounded()) << 8
                | UInt32((c.blueComponent * 255).rounded())
        }
        #expect(try hex(bg, .darkAqua) == 0x0B0B14)
        #expect(try hex(bg, .aqua) == 0xF3F2EE)
        #expect(try hex(KColor.ns(\.accent), .aqua) == 0x0B0B14)
        #expect(try hex(KColor.indigo, .aqua) == 0x818CF8)
    }

    @Test func fontsAreRegisteredAtLaunch() {
        #expect(KFont.register())
        #expect(KFont.isAvailable(KFont.geistName))
        #expect(KFont.isAvailable(KFont.geistMonoName))
        #expect(KFont.isAvailable(KnodeFonts.blockName))
        #expect(KFont.isAvailable(KnodeFonts.lineName))
        #expect(KFont.nsUI(13).fontName == KFont.geistName)
        #expect(KFont.nsMono(13).fontName == KFont.geistMonoName)
    }

    @Test func geistWeightsUseTheVariableAxis() throws {
        let font = KFont.nsUI(15, .semibold)
        #expect(font.familyName == "Geist")
        let variation = try #require(CTFontCopyVariation(font as CTFont) as? [NSNumber: NSNumber])
        #expect(variation[NSNumber(value: 0x7767_6874)]?.doubleValue == 600)
    }
}
