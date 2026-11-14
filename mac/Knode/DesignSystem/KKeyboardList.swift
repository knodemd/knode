import AppKit
import SwiftUI

/// Where a key moves the selection of a list.
enum KListMove: Equatable {
    case previous, next, first, last

    /// The move a key stands for: Up and Down step, Home and End or Command-Up and Command-Down
    /// jump to the ends. Nil for any other key.
    init?(key: KeyEquivalent, modifiers: EventModifiers = []) {
        let jump = modifiers.contains(.command)
        switch key {
        case .upArrow: self = jump ? .first : .previous
        case .downArrow: self = jump ? .last : .next
        case .home: self = .first
        case .end: self = .last
        default: return nil
        }
    }
}

/// Moving the selection of a list with the keyboard, apart from any view so it can be tested.
enum KListNavigation {
    /// The index a move lands on in a list of `count` items, clamped to the list. With nothing
    /// selected, Down starts at the top and Up at the bottom. Nil when the list is empty.
    static func index(after move: KListMove, from current: Int?, count: Int) -> Int? {
        guard count > 0 else { return nil }
        switch move {
        case .first: return 0
        case .last: return count - 1
        case .next: return current.map { min($0 + 1, count - 1) } ?? 0
        case .previous: return current.map { max($0 - 1, 0) } ?? count - 1
        }
    }

    /// The item a move lands on. A selection no longer in the list counts as no selection.
    static func target<ID: Hashable>(after move: KListMove, from current: ID?, in ids: [ID]) -> ID? {
        let index = current.flatMap { ids.firstIndex(of: $0) }
        return Self.index(after: move, from: index, count: ids.count).map { ids[$0] }
    }

    /// How to scroll to the row a move landed on: the ends of the list show at its edge, a step
    /// scrolls only as far as needed to show the whole row (nil anchor).
    static func scrollAnchor(for move: KListMove?) -> UnitPoint? {
        switch move {
        case .first: .top
        case .last: .bottom
        case .previous, .next, nil: nil
        }
    }
}

extension View {
    /// Makes a list in a scroll view move its selection with the keyboard while it has focus:
    /// Up and Down, Home and End, Command-Up and Command-Down; Return calls `activate`. The
    /// selected row scrolls into view whenever the selection changes. Keys only reach the list
    /// while it has focus, so typing in the editor or a field never moves it. Each row needs
    /// `.id(_:)` with its ID, and shows the focus ring itself (`KRow(isFocused:)`), never the
    /// system one.
    func kKeyboardList<ID: Hashable>(
        ids: [ID],
        selection: ID?,
        focus: FocusState<Bool>.Binding,
        select: @escaping (ID) -> Void,
        activate: @escaping (ID) -> Void
    ) -> some View {
        modifier(KKeyboardList(ids: ids, selection: selection, focus: focus, select: select, activate: activate))
    }
}

private struct KKeyboardList<ID: Hashable>: ViewModifier {
    let ids: [ID]
    let selection: ID?
    let focus: FocusState<Bool>.Binding
    let select: (ID) -> Void
    let activate: (ID) -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var lastMove: KListMove?

    func body(content: Content) -> some View {
        ScrollViewReader { proxy in
            content
                .focusable()
                .focusEffectDisabled()
                .focused(focus)
                .onKeyPress(keys: [.upArrow, .downArrow, .home, .end], phases: [.down, .repeat]) { press in
                    guard let move = KListMove(key: press.key, modifiers: press.modifiers),
                          let target = KListNavigation.target(after: move, from: selection, in: ids)
                    else { return .ignored }
                    lastMove = move
                    if target != selection { select(target) } else { scroll(to: target, proxy) }
                    return .handled
                }
                .onKeyPress(.return) {
                    guard let selection, ids.contains(selection) else { return .ignored }
                    activate(selection)
                    return .handled
                }
                .onChange(of: selection) { _, id in
                    if let id { scroll(to: id, proxy) }
                }
        }
    }

    private func scroll(to id: ID, _ proxy: ScrollViewProxy) {
        let anchor = KListNavigation.scrollAnchor(for: lastMove)
        lastMove = nil
        withAnimation(KMotion.animation(reduceMotion: reduceMotion)) { proxy.scrollTo(id, anchor: anchor) }
    }
}
