import SwiftUI
import Testing
@testable import Knode

struct ListNavigationTests {
    @Test func keysMapToMoves() {
        #expect(KListMove(key: .upArrow) == .previous)
        #expect(KListMove(key: .downArrow) == .next)
        #expect(KListMove(key: .home) == .first)
        #expect(KListMove(key: .end) == .last)
        #expect(KListMove(key: .upArrow, modifiers: .command) == .first)
        #expect(KListMove(key: .downArrow, modifiers: .command) == .last)
        #expect(KListMove(key: .leftArrow) == nil)
        #expect(KListMove(key: .return) == nil)
        #expect(KListMove(key: "a") == nil)
    }

    @Test func stepsMoveOneRowAndStopAtTheEnds() {
        #expect(KListNavigation.index(after: .next, from: 1, count: 4) == 2)
        #expect(KListNavigation.index(after: .previous, from: 1, count: 4) == 0)
        #expect(KListNavigation.index(after: .next, from: 3, count: 4) == 3)
        #expect(KListNavigation.index(after: .previous, from: 0, count: 4) == 0)
    }

    @Test func jumpsGoToTheEnds() {
        #expect(KListNavigation.index(after: .first, from: 2, count: 4) == 0)
        #expect(KListNavigation.index(after: .last, from: 0, count: 4) == 3)
        #expect(KListNavigation.index(after: .first, from: nil, count: 4) == 0)
        #expect(KListNavigation.index(after: .last, from: nil, count: 4) == 3)
    }

    @Test func withNothingSelectedDownStartsAtTheTopAndUpAtTheBottom() {
        #expect(KListNavigation.index(after: .next, from: nil, count: 3) == 0)
        #expect(KListNavigation.index(after: .previous, from: nil, count: 3) == 2)
    }

    @Test func anEmptyListHasNoTarget() {
        for move in [KListMove.previous, .next, .first, .last] {
            #expect(KListNavigation.index(after: move, from: nil, count: 0) == nil)
            #expect(KListNavigation.target(after: move, from: "a", in: [String]()) == nil)
        }
    }

    @Test func aSelectionOutOfRangeIsClamped() {
        #expect(KListNavigation.index(after: .next, from: 9, count: 3) == 2)
        #expect(KListNavigation.index(after: .previous, from: -4, count: 3) == 0)
    }

    @Test func targetsFollowTheIDsInOrder() {
        let ids = ["a", "b", "c"]
        #expect(KListNavigation.target(after: .next, from: "a", in: ids) == "b")
        #expect(KListNavigation.target(after: .previous, from: "c", in: ids) == "b")
        #expect(KListNavigation.target(after: .next, from: "c", in: ids) == "c")
        #expect(KListNavigation.target(after: .last, from: "a", in: ids) == "c")
        // A selection that left the list, like an open note a search does not find.
        #expect(KListNavigation.target(after: .next, from: "z", in: ids) == "a")
        #expect(KListNavigation.target(after: .previous, from: "z", in: ids) == "c")
    }

    @Test func endsScrollToTheEdgeAndStepsOnlyAsFarAsNeeded() {
        #expect(KListNavigation.scrollAnchor(for: .first) == .top)
        #expect(KListNavigation.scrollAnchor(for: .last) == .bottom)
        #expect(KListNavigation.scrollAnchor(for: .next) == nil)
        #expect(KListNavigation.scrollAnchor(for: .previous) == nil)
        #expect(KListNavigation.scrollAnchor(for: nil) == nil)
    }
}
