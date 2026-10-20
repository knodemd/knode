import SwiftUI

/// A one-pixel line in the line colour, across or down.
struct KDivider: View {
    var axis: Axis = .horizontal
    @Environment(\.kTheme) private var theme

    init(_ axis: Axis = .horizontal) { self.axis = axis }

    var body: some View {
        Rectangle()
            .fill(theme.line)
            .frame(width: axis == .vertical ? 1 : nil, height: axis == .horizontal ? 1 : nil)
            .accessibilityHidden(true)
    }
}
