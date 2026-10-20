/// Icons the app needs that the brand set does not have yet, drawn on the same 7 x 7 grid with
/// the same half-cell corners. Each one is proposed for brand.json; once the brand ships it,
/// the app switches to the brand's and the drawing here goes.
enum KnodeAppIcon: String, CaseIterable, Sendable {
    /// Attachments: a paper clip.
    case attach
    /// A canvas: cards placed freely on a board.
    case canvas
    /// The editor split in two: source beside preview.
    case split
    /// The inspector: a panel on the right side of the window.
    case inspector
    /// Key hints: the Command, Shift, Option and Return keys.
    case keyCommand
    case keyShift
    case keyOption
    case keyReturn

    var rows: [String] {
        switch self {
        case .attach: [".#####.", ".#...#.", ".#.#.#.", ".#.#.#.", ".#.#.#.", ".#.###.", ".#....."]
        case .canvas: ["###.###", "#.#.###", "###....", ".......", "..#####", "..#...#", "..#####"]
        case .split: ["#######", "#..#..#", "#..#..#", "#..#..#", "#..#..#", "#..#..#", "#######"]
        case .inspector: ["#######", "#...#.#", "#...#.#", "#...#.#", "#...#.#", "#...#.#", "#######"]
        case .keyCommand: ["###.###", "#.#.#.#", "#######", "..#.#..", "#######", "#.#.#.#", "###.###"]
        case .keyShift: ["...#...", "..###..", ".##.##.", "##...##", ".#...#.", ".#...#.", ".#####."]
        case .keyOption: ["###.###", "..#....", "..##...", "...#...", "...##..", "....#..", "....###"]
        case .keyReturn: ["......#", "......#", "..#...#", ".##...#", "#######", ".##....", "..#...."]
        }
    }

    var title: String {
        switch self {
        case .attach: "Attach"
        case .canvas: "Canvas"
        case .split: "Split"
        case .inspector: "Inspector"
        case .keyCommand: "Command"
        case .keyShift: "Shift"
        case .keyOption: "Option"
        case .keyReturn: "Return"
        }
    }
}

extension KGlyph {
    static let attach = KGlyph(KnodeAppIcon.attach)
    static let canvas = KGlyph(KnodeAppIcon.canvas)
    static let split = KGlyph(KnodeAppIcon.split)
    static let inspector = KGlyph(KnodeAppIcon.inspector)
    static let keyCommand = KGlyph(KnodeAppIcon.keyCommand)
    static let keyShift = KGlyph(KnodeAppIcon.keyShift)
    static let keyOption = KGlyph(KnodeAppIcon.keyOption)
    static let keyReturn = KGlyph(KnodeAppIcon.keyReturn)
}
