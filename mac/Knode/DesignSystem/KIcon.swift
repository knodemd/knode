import KnodeBrand
import SwiftUI

/// A drawing on the 7 x 7 Knode grid: one of the brand's icons or one drawn for the app.
struct KGlyph: Hashable, Sendable {
    let rows: [String]
    let title: String

    init(rows: [String], title: String) {
        self.rows = rows
        self.title = title
    }

    init(_ icon: KnodeIcon) { self.init(rows: icon.rows, title: icon.title) }
}

/// Icon sizes that keep every cell on whole pixels at 2x: 1.5, 2, 2.5, 3 and 4 points a cell.
enum KIconSize: CGFloat, Sendable {
    case xs = 10.5
    case sm = 14
    case md = 17.5
    case lg = 21
    case xl = 28
}

/// A Knode icon, tinted with the foreground style unless a colour is given.
struct KIcon: View {
    var glyph: KGlyph
    var size: CGFloat
    var color: Color?

    init(_ glyph: KGlyph, size: KIconSize = .sm, color: Color? = nil) {
        self.glyph = glyph
        self.size = size.rawValue
        self.color = color
    }

    init(_ glyph: KGlyph, points: CGFloat, color: Color? = nil) {
        self.glyph = glyph
        self.size = points
        self.color = color
    }

    var body: some View {
        Group {
            if let color {
                KnodeGridShape(rows: glyph.rows).fill(color, style: FillStyle(eoFill: true))
            } else {
                KnodeGridShape(rows: glyph.rows).fill(.foreground, style: FillStyle(eoFill: true))
            }
        }
        .frame(width: size, height: size)
        .accessibilityLabel(glyph.title)
    }
}

// Every brand icon, by its name in brand.json.
extension KGlyph {
    static let node = KGlyph(KnodeIcon.node)
    static let knode = KGlyph(KnodeIcon.knode)
    static let connect = KGlyph(KnodeIcon.connect)
    static let map = KGlyph(KnodeIcon.map)
    static let `import` = KGlyph(KnodeIcon.`import`)
    static let export = KGlyph(KnodeIcon.export)
    static let file = KGlyph(KnodeIcon.file)
    static let folder = KGlyph(KnodeIcon.folder)
    static let search = KGlyph(KnodeIcon.search)
    static let chat = KGlyph(KnodeIcon.chat)
    static let settings = KGlyph(KnodeIcon.settings)
    static let add = KGlyph(KnodeIcon.add)
    static let delete = KGlyph(KnodeIcon.delete)
    static let profile = KGlyph(KnodeIcon.profile)
    static let save = KGlyph(KnodeIcon.save)
    static let note = KGlyph(KnodeIcon.note)
    static let link = KGlyph(KnodeIcon.link)
    static let home = KGlyph(KnodeIcon.home)
    static let list = KGlyph(KnodeIcon.list)
    static let grid = KGlyph(KnodeIcon.grid)
    static let menu = KGlyph(KnodeIcon.menu)
    static let more = KGlyph(KnodeIcon.more)
    static let close = KGlyph(KnodeIcon.close)
    static let done = KGlyph(KnodeIcon.done)
    static let attach = KGlyph(KnodeIcon.attach)
    static let canvas = KGlyph(KnodeIcon.canvas)
    static let split = KGlyph(KnodeIcon.split)
    static let inspector = KGlyph(KnodeIcon.inspector)
    static let keyCommand = KGlyph(KnodeIcon.keyCommand)
    static let keyShift = KGlyph(KnodeIcon.keyShift)
    static let keyOption = KGlyph(KnodeIcon.keyOption)
    static let keyReturn = KGlyph(KnodeIcon.keyReturn)
    static let edit = KGlyph(KnodeIcon.edit)
    static let back = KGlyph(KnodeIcon.back)
    static let forward = KGlyph(KnodeIcon.forward)
    static let filter = KGlyph(KnodeIcon.filter)
    static let tag = KGlyph(KnodeIcon.tag)
    static let history = KGlyph(KnodeIcon.history)
    static let copy = KGlyph(KnodeIcon.copy)
    static let show = KGlyph(KnodeIcon.show)
    static let `private` = KGlyph(KnodeIcon.`private`)
    static let cloud = KGlyph(KnodeIcon.cloud)
    static let alerts = KGlyph(KnodeIcon.alerts)
    static let calendar = KGlyph(KnodeIcon.calendar)
    static let image = KGlyph(KnodeIcon.image)
    static let focus = KGlyph(KnodeIcon.focus)
    static let paste = KGlyph(KnodeIcon.paste)
    static let cut = KGlyph(KnodeIcon.cut)
    static let undo = KGlyph(KnodeIcon.undo)
    static let redo = KGlyph(KnodeIcon.redo)
    static let select = KGlyph(KnodeIcon.select)
    static let group = KGlyph(KnodeIcon.group)
    static let zoomIn = KGlyph(KnodeIcon.zoomIn)
    static let zoomOut = KGlyph(KnodeIcon.zoomOut)
    static let expand = KGlyph(KnodeIcon.expand)
    static let sidebar = KGlyph(KnodeIcon.sidebar)
    static let tree = KGlyph(KnodeIcon.tree)
    static let pin = KGlyph(KnodeIcon.pin)
    static let hide = KGlyph(KnodeIcon.hide)
    static let text = KGlyph(KnodeIcon.text)
    static let tasks = KGlyph(KnodeIcon.tasks)
    static let info = KGlyph(KnodeIcon.info)
    static let warning = KGlyph(KnodeIcon.warning)
    static let help = KGlyph(KnodeIcon.help)
    static let up = KGlyph(KnodeIcon.up)
    static let down = KGlyph(KnodeIcon.down)
    static let sort = KGlyph(KnodeIcon.sort)
    static let people = KGlyph(KnodeIcon.people)
    static let bookmark = KGlyph(KnodeIcon.bookmark)
    static let archive = KGlyph(KnodeIcon.archive)
    static let drag = KGlyph(KnodeIcon.drag)
    static let web = KGlyph(KnodeIcon.web)
    static let mail = KGlyph(KnodeIcon.mail)
    static let rss = KGlyph(KnodeIcon.rss)
    static let code = KGlyph(KnodeIcon.code)
    static let terminal = KGlyph(KnodeIcon.terminal)
    static let copyright = KGlyph(KnodeIcon.copyright)
    static let license = KGlyph(KnodeIcon.license)
}
