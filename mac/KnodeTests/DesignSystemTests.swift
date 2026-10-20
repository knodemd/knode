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

    @Test func appIconsAreDrawnOnTheSevenBySevenGrid() {
        for icon in KnodeAppIcon.allCases {
            #expect(icon.rows.count == 7, "\(icon)")
            #expect(icon.rows.allSatisfy { $0.count == 7 && $0.allSatisfy { $0 == "#" || $0 == "." } }, "\(icon)")
            #expect(!KnodeGrid.path(icon.rows).isEmpty, "\(icon)")
        }
        // An app icon never shadows a brand icon: once the brand has it, the app uses the brand's.
        let brand = Set(KnodeIcon.allCases.map(\.rawValue))
        #expect(KnodeAppIcon.allCases.allSatisfy { !brand.contains($0.rawValue) })
    }

    @Test @MainActor func popoversOpenWhereAskedWhenTheyFit() {
        let m = KFloatingPresenter.margin
        let window = NSRect(x: 0, y: 0, width: 1200, height: 800)
        let button = NSRect(x: 100, y: 600, width: 80, height: 30)
        let size = NSSize(width: 300, height: 200)
        let below = KFloatingPresenter.origin(size: size, anchor: button, edge: .below, point: nil, window: window, screen: nil)
        #expect(below == NSPoint(x: button.minX - m, y: button.minY - 6 - size.height + m))
        let leading = KFloatingPresenter.origin(size: size, anchor: NSRect(x: 900, y: 600, width: 80, height: 30),
                                                edge: .leading, point: nil, window: window, screen: nil)
        #expect(leading.y == 630 - size.height + m)
    }

    @Test @MainActor func aPopoverBelowAButtonNearTheRightEdgeLinesUpWithItsRightEdge() {
        let m = KFloatingPresenter.margin
        let window = NSRect(x: 0, y: 0, width: 1200, height: 800)
        let button = NSRect(x: 1050, y: 740, width: 100, height: 30)
        let size = NSSize(width: 360, height: 400)
        let origin = KFloatingPresenter.origin(size: size, anchor: button, edge: .below, point: nil, window: window, screen: nil)
        #expect(origin.x + size.width - m == button.maxX)
        #expect(origin.x + size.width - m <= window.maxX)
    }

    @Test @MainActor func aPopoverBesideAButtonNearTheBottomGrowsUp() {
        let m = KFloatingPresenter.margin
        let window = NSRect(x: 0, y: 0, width: 1200, height: 800)
        let button = NSRect(x: 900, y: 20, width: 280, height: 36)
        let size = NSSize(width: 340, height: 560)
        let origin = KFloatingPresenter.origin(size: size, anchor: button, edge: .leading, point: nil, window: window, screen: nil)
        #expect(origin.y + m == button.minY)
        #expect(origin.y + size.height - m <= window.maxY)
    }

    @Test @MainActor func popoversStayOnTheScreen() {
        let screen = NSRect(x: 0, y: 0, width: 1000, height: 700)
        let window = NSRect(x: 600, y: 0, width: 1200, height: 800)
        let button = NSRect(x: 950, y: 300, width: 40, height: 30)
        let size = NSSize(width: 300, height: 200)
        let origin = KFloatingPresenter.origin(size: size, anchor: button, edge: .trailing, point: nil, window: window, screen: screen)
        #expect(origin.x + size.width <= screen.maxX)
    }
}
