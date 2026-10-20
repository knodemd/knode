import AppKit
import SwiftUI

/// The Markdown source of the open note, in an `NSTextView`.
///
/// Each edit the person makes is reported to the editor with the range it replaced, so it is
/// sent as a splice. Text that changes under it, written by someone else, is patched in place:
/// only the runs that differ are replaced, and the selection moves with the text around it.
/// With `attachments`, files dropped in the text and images pasted into it are attached and
/// written as `![[name]]` where they went.
struct MarkdownSourceEditor: NSViewRepresentable {
    let editor: NoteEditorModel
    var attachments: AttachmentDrop?

    /// Geist Mono, the type of the source on knode.md.
    static var font: NSFont { KFont.nsMono(13.5) }

    /// Lines a little apart, like the web's code blocks.
    static var paragraphStyle: NSParagraphStyle {
        let style = NSMutableParagraphStyle()
        style.lineSpacing = 5
        return style
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(editor: editor)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.scrollerStyle = .overlay
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        scroll.drawsBackground = false
        let textView = AttachingTextView()
        textView.autoresizingMask = [.width]
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        textView.textContainer?.widthTracksTextView = true
        textView.attachments = attachments
        scroll.documentView = textView
        textView.delegate = context.coordinator
        textView.isRichText = false
        textView.importsGraphics = false
        textView.allowsUndo = true
        textView.drawsBackground = false
        textView.font = Self.font
        textView.typingAttributes = Coordinator.attributes
        textView.textContainerInset = NSSize(width: 23, height: 20)
        // The caret and the selection in indigo, never the system accent colour.
        textView.insertionPointColor = KColor.indigo
        textView.selectedTextAttributes = [
            .backgroundColor: KColor.indigo.withAlphaComponent(0.32),
            .foregroundColor: KColor.ns(\.fg),
        ]
        textView.linkTextAttributes = [.foregroundColor: KColor.indigo]
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticDashSubstitutionEnabled = false
        textView.isAutomaticTextReplacementEnabled = false
        textView.string = editor.body
        textView.textStorage?.setAttributes(Coordinator.attributes, range: NSRange(location: 0, length: (editor.body as NSString).length))
        textView.setAccessibilityLabel("Markdown")
        context.coordinator.textView = textView
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.editor = editor
        (context.coordinator.textView as? AttachingTextView)?.attachments = attachments
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
            [.font: MarkdownSourceEditor.font, .foregroundColor: KColor.ns(\.fg), .paragraphStyle: MarkdownSourceEditor.paragraphStyle]
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
