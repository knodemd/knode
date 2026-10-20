#if DEBUG
import KnodeBrand
import SwiftUI

/// Every component of the design system, in both appearances. Debug builds only: Window >
/// Design Gallery.
struct DesignGallery: View {
    @State private var scheme: ColorScheme = .dark

    var body: some View {
        DesignGalleryPage(scheme: $scheme)
            .environment(\.colorScheme, scheme)
            .knodeTheme(background: true)
            .frame(minWidth: 760, minHeight: 600)
    }
}

private enum GalleryMode: Hashable { case source, split, preview }
private enum GalleryTab: Hashable { case links, attachments, history }

private struct DesignGalleryPage: View {
    @Binding var scheme: ColorScheme
    @Environment(\.kTheme) private var theme
    @State private var mode = GalleryMode.split
    @State private var tab = GalleryTab.links
    @State private var text = ""
    @State private var search = ""
    @State private var on = true
    @State private var off = false
    @State private var checked = true
    @State private var unchecked = false
    @State private var choice = "Newest first"
    @State private var popover = false
    @State private var sheet = false
    @State private var confirm: KConfirmation?
    @State private var selectedRow = 1
    @State private var toasts = KToastCenter()

    var body: some View {
        KScrollView {
            VStack(alignment: .leading, spacing: KSpace.x8) {
                header
                section("Type") { type }
                section("Icons") { icons }
                section("Buttons") { buttons }
                section("Choices") { choices }
                section("Fields") { fields }
                section("Lists") { lists }
                section("Cards, chips and progress") { cards }
                section("Overlays") { overlays }
                section("Banners and empty states") { banners }
            }
            .padding(KSpace.x10)
            .frame(maxWidth: 980, alignment: .leading)
        }
        .kSheet(isPresented: $sheet) {
            VStack(alignment: .leading, spacing: KSpace.x5) {
                KSheetHeader(kicker: "New space", title: "Name the space", message: "A space is a folder of notes that agents can read and write.")
                KTextField("Name", text: $text)
                HStack {
                    Spacer()
                    KButton("Cancel") { sheet = false }
                    KButton("Create", icon: .add, kind: .primary) { sheet = false }
                }
            }
        }
        .kConfirm($confirm)
        .kToasts(toasts)
    }

    private var header: some View {
        HStack(alignment: .center) {
            VStack(alignment: .leading, spacing: KSpace.x2) {
                Text("Design system").kLineStyle(13).foregroundStyle(theme.isDark ? theme.indigo : theme.muted)
                Text("Knode for Mac").kBlockStyle(40)
            }
            Spacer()
            KSegmented(selection: $scheme, options: [KOption(.dark, "Ink"), KOption(.light, "Paper")])
        }
    }

    private func section<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: KSpace.x4) {
            KSectionLabel(title)
            content()
        }
    }

    private var type: some View {
        VStack(alignment: .leading, spacing: KSpace.x3) {
            Text("Connect your thoughts.").kBlockStyle(28)
            Text("Imported conversations").kLineStyle(13).foregroundStyle(theme.muted)
            Text("A note title in Geist semibold").font(KFont.ui(22, .semibold))
            Text("Body text in Geist: notes, settings and everything you read in the app.").font(KFont.ui(14)).foregroundStyle(theme.muted)
            Text("knode search \"meaning\" --space ~/Knode/Work.knode").font(KFont.mono(13))
        }
    }

    private var icons: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 40), spacing: KSpace.x2)], alignment: .leading, spacing: KSpace.x3) {
            ForEach(KnodeIcon.allCases, id: \.self) { icon in
                KIcon(KGlyph(icon), size: .md).kTooltip(icon.title)
            }
        }
    }

    private var buttons: some View {
        VStack(alignment: .leading, spacing: KSpace.x3) {
            HStack(spacing: KSpace.x2) {
                KButton("New note", icon: .add, kind: .primary) {}
                KButton("Open space", icon: .folder) {}
                KButton("Show graph", icon: .map, kind: .ghost) {}
                KButton("Delete", icon: .delete, kind: .destructive) {}
                KButton("Disabled", icon: .hide) {}.disabled(true)
            }
            HStack(spacing: KSpace.x2) {
                KButton("Small", icon: .add, kind: .primary, size: .small) {}
                KButton("Large", icon: .import, kind: .primary, size: .large) {}
                KIconButton(.sidebar, title: "Hide sidebar") {}
                KIconButton(.link, title: "Links", isActive: true) {}
                KIconButton(.history, title: "History") {}
                KIconButton(.more, title: "More") {}
                Button("A text link") {}.buttonStyle(KLinkButtonStyle())
            }
        }
    }

    private var choices: some View {
        VStack(alignment: .leading, spacing: KSpace.x4) {
            HStack(spacing: KSpace.x4) {
                KSegmented(selection: $mode, options: [KOption(.source, "Source", icon: .code), KOption(.split, "Split", icon: .split), KOption(.preview, "Preview", icon: .show)])
                KSegmented(selection: $mode, options: [KOption(.source, "Source", icon: .code), KOption(.split, "Split", icon: .split), KOption(.preview, "Preview", icon: .show)], showsTitles: false)
                KDropdown(selection: $choice, options: ["Newest first", "Oldest first", "By title"].map { KOption($0, $0) })
                    .frame(width: 180)
            }
            KTabs(selection: $tab, options: [KOption(.links, "Links", icon: .link), KOption(.attachments, "Attachments", icon: .attach), KOption(.history, "History", icon: .history)])
            HStack(spacing: KSpace.x6) {
                KToggle("Capture conversations", isOn: $on).frame(width: 240)
                KToggle("Off", isOn: $off).frame(width: 120)
                KCheckbox("Done", isOn: $checked)
                KCheckbox("To do", isOn: $unchecked)
            }
        }
    }

    private var fields: some View {
        HStack(spacing: KSpace.x3) {
            KTextField("Space name", text: $text).frame(width: 220)
            KTextField("~/Knode", text: $text, icon: .folder, mono: true).frame(width: 220)
            KSearchField(text: $search, hint: [.command, .char("F")]).frame(width: 240)
        }
    }

    private var lists: some View {
        KCard(padding: KSpace.x2) {
            VStack(spacing: 2) {
                ForEach(0..<4) { index in
                    KRow(isSelected: index == selectedRow, action: { selectedRow = index }) {
                        VStack(alignment: .leading, spacing: 3) {
                            HStack {
                                Text(["Weekly review", "Reading list", "Ideas for the graph", "Meeting with design"][index])
                                    .font(KFont.ui(13, .semibold))
                                Spacer()
                                Text("\(index + 2)H").kLineStyle(10).foregroundStyle(theme.muted)
                            }
                            Text("What went well, what to change, what comes next.").font(KFont.ui(12)).foregroundStyle(theme.muted).lineLimit(1)
                        }
                    }
                    .kContextMenu {
                        [.action("Open", icon: .note) {}, .action("Pin", icon: .pin) {}, .divider,
                         .action("Delete", icon: .delete, isDestructive: true) {}]
                    }
                }
            }
        }
        .frame(width: 340)
    }

    private var cards: some View {
        HStack(alignment: .top, spacing: KSpace.x4) {
            KCard {
                VStack(alignment: .leading, spacing: KSpace.x3) {
                    Text("Plan").kBlockStyle(19)
                    Text("A card: panel colour, a hairline, 18 pt corners.").font(KFont.ui(13)).foregroundStyle(theme.muted)
                    HStack { KChip("Default", icon: .done, kind: .accent); KChip("Agent"); KChip("New", kind: .solid) }
                }
            }
            KCard(highlighted: true) {
                VStack(alignment: .leading, spacing: KSpace.x3) {
                    Text("Indexing").kLineStyle(11).foregroundStyle(theme.muted)
                    KProgress(value: 0.62)
                    KProgress(value: nil)
                    HStack(spacing: KSpace.x2) { KSpinner(); Text("Finding notes by meaning").font(KFont.ui(12)).foregroundStyle(theme.muted) }
                }
            }
        }
    }

    private var overlays: some View {
        HStack(spacing: KSpace.x2) {
            KButton("Popover", icon: .info) { popover.toggle() }
                .kPopover(isPresented: $popover) {
                    VStack(alignment: .leading, spacing: KSpace.x2) {
                        Text("Popover").kLineStyle(11).foregroundStyle(theme.muted)
                        Text("Our card in a floating panel.").font(KFont.ui(13))
                    }
                    .frame(width: 220, alignment: .leading)
                }
            KMenuButton(style: .secondary, entries: {
                [.section("Sort"), .action("Newest first", isChecked: true) {}, .action("By title") {}, .divider,
                 .action("New note", icon: .add, shortcut: [.command, .char("N")]) {}]
            }, label: {
                HStack(spacing: 6) { KIcon(.sort, size: .sm); Text("Menu") }
            })
            KButton("Sheet", icon: .note) { sheet = true }
            KButton("Confirm", icon: .delete, kind: .destructive) {
                confirm = KConfirmation(title: "Delete this note?", message: "It stays in the history of the space and can be restored.",
                                        confirmTitle: "Delete", isDestructive: true) {}
            }
            KButton("Toast", icon: .done) { toasts.show("Note saved") }
            KKeyHint([.command, .shift, .char("M")])
            KKeyHint([.option, .return])
        }
    }

    private var banners: some View {
        VStack(alignment: .leading, spacing: KSpace.x3) {
            KBanner(.info, title: "Knode Service is not running", message: "Agents cannot reach your spaces until it starts.", onClose: {}) {
                KButton("Start", kind: .primary, size: .small) {}
            }
            KBanner(.warning, title: "3 notes were deleted from the Markdown folder")
            KBanner(.error, title: "The space could not be opened", message: "manifest.json is missing.")
            KCard(padding: 0) {
                KEmptyState(icon: .note, title: "No note open", message: "Choose a note in the sidebar or start a new one.") {
                    KButton("New note", icon: .add, kind: .primary) {}
                }
                .frame(height: 260)
            }
        }
    }
}

/// Window > Design Gallery, in Debug builds.
struct DesignGalleryCommands: Commands {
    @Environment(\.openWindow) private var openWindow

    var body: some Commands {
        CommandGroup(after: .windowList) {
            Button("Design Gallery") { openWindow(id: DesignGallery.windowID) }
                .keyboardShortcut("g", modifiers: [.command, .option, .shift])
        }
    }
}

extension DesignGallery {
    static let windowID = "design-gallery"
}

#Preview("Design gallery") { DesignGallery() }
#endif
