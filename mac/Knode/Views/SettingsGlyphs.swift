// Icons this part of the app needs that the brand set does not have yet, drawn on the 7 x 7 Knode
// grid with the same half-cell corners. Each one is to be proposed for brand.json.

extension KGlyph {
    /// Start: a triangle pointing right.
    static let play = KGlyph(rows: ["..#....", "..##...", "..###..", "..####.", "..###..", "..##...", "..#...."], title: "Start")
    /// Stop: a square.
    static let stop = KGlyph(rows: [".......", ".#####.", ".#####.", ".#####.", ".#####.", ".#####.", "......."], title: "Stop")
    /// A key, for an agent's key and revoking it.
    static let key = KGlyph(rows: [".###...", "##.##..", ".###...", "..#....", "..##...", "..#....", "..##..."], title: "Key")
    /// Refresh: an arrow going round.
    static let refresh = KGlyph(rows: ["..###.#", ".#...##", "#...###", "#......", "#.....#", ".#...#.", "..###.."], title: "Refresh")
    /// Power, for starting at login and quitting.
    static let power = KGlyph(rows: ["...#...", ".#.#.#.", "#..#..#", "#.....#", "#.....#", ".#...#.", "..###.."], title: "Power")
}
