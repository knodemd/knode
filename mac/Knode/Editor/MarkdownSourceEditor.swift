import AppKit
import SwiftUI

/// The Markdown source of the open note, in an `NSTextView`.
///
/// Each edit the person makes is reported to the editor with the range it replaced, so it is
/// sent as a splice. Text that changes under it, written by someone else, is patched in place:
/// only the runs that differ are replaced, and the selection moves with the text around it.
struct MarkdownSourceEditor: NSViewRepresentable {
    let editor: NoteEditorModel

    static let font = NSFont.monospacedSystemFont(ofSize: 14, weight: .regular)

    func makeCoordinator() -> Coordinator {
        Coordinator(editor: editor)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSTextView.scrollableTextView()
        scroll.drawsBackground = false
        let textView = scroll.documentView as! NSTextView
        textView.delegate = context.coordinator
        textView.isRichText = false
        textView.importsGraphics = false
        textView.allowsUndo = true
        textView.drawsBackground = false
        textView.font = Self.font
        textView.typingAttributes = Coordinator.attributes
        textView.textContainerInset = NSSize(width: 18, height: 12)
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticDashSubstitutionEnabled = false
        textView.isAutomaticTextReplacementEnabled = false
        textView.string = editor.body
        textView.setAccessibilityLabel("Markdown")
        context.coordinator.textView = textView
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.editor = editor
        // Read here, so the view updates when the body changes.
        context.coordinator.show(editor.body)
    }

    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        var editor: NoteEditorModel
        weak var textView: NSTextView?
        /// The edit the text view is about to make.
        private var change: (range: NSRange, replacement: String)?
        private var patching = false

        static var attributes: [NSAttributedString.Key: Any] {
            [.font: MarkdownSourceEditor.font, .foregroundColor: NSColor.textColor]
        }

        init(editor: NoteEditorModel) {
            self.editor = editor
        }

        func textView(_ textView: NSTextView, shouldChangeTextIn range: NSRange, replacementString: String?) -> Bool {
            if !patching, let replacementString { change = (range, replacementString) }
            return true
        }

        func textDidChange(_ notification: Notification) {
            guard !patching, let textView else { return }
            let change = self.change
            self.change = nil
            editor.isComposing = textView.hasMarkedText()
            editor.viewDidEdit(text: textView.string, range: change?.range, replacement: change?.replacement)
        }

        /// Shows `text`, patching only what differs and moving the selection with it. Waits while
        /// an input method composes text.
        func show(_ text: String) {
            guard let textView, let storage = textView.textStorage, textView.string != text, !textView.hasMarkedText() else { return }
            let patches = TextDiff.patches(from: textView.string, to: text)
            let selection = textView.selectedRanges.map { TextDiff.transform($0.rangeValue, through: patches) }
            patching = true
            defer { patching = false }
            storage.beginEditing()
            for patch in patches.reversed() {
                storage.replaceCharacters(in: patch.range, with: NSAttributedString(string: patch.replacement, attributes: Self.attributes))
            }
            storage.endEditing()
            textView.selectedRanges = selection.map { NSValue(range: $0) }
            // The undo stack holds ranges of the text before: the person's own edits are undone
            // through the history instead.
            textView.undoManager?.removeAllActions(withTarget: textView)
            textView.undoManager?.removeAllActions(withTarget: storage)
        }
    }
}
