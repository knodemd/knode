import AppKit

/// Keyboard focus between the sidebar lists and the note editor.
enum KFocus {
    /// True while the key window's first responder is a text view or field: then nothing should
    /// take the focus away on its own.
    @MainActor static var isEditingText: Bool {
        NSApp.keyWindow?.firstResponder is NSText
    }

    /// Puts the caret in the Markdown source of the open note, if it shows. Returns false when no
    /// source editor is on screen (for example in Preview).
    @MainActor @discardableResult
    static func focusNoteEditor() -> Bool {
        guard let window = NSApp.keyWindow, let root = window.contentView,
              let textView = firstSourceEditor(in: root)
        else { return false }
        return window.makeFirstResponder(textView)
    }

    @MainActor private static func firstSourceEditor(in view: NSView) -> NSTextView? {
        if let textView = view as? AttachingTextView, textView.isEditable, !textView.isHiddenOrHasHiddenAncestor {
            return textView
        }
        for child in view.subviews {
            if let found = firstSourceEditor(in: child) { return found }
        }
        return nil
    }
}
