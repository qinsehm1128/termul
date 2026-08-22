import SwiftTerm
import SwiftUI
import UIKit

struct TerminalScreen: UIViewRepresentable {
    var terminalId: String
    var buffered: Data
    var onSend: (String) -> Void
    var onResize: (Int, Int) -> Void
    var onReady: (@escaping (Data) -> Void) -> Void
    var focusToken: UInt64 = 0

    func makeCoordinator() -> Coordinator {
        Coordinator(onSend: onSend, onResize: onResize)
    }

    func makeUIView(context: Context) -> TerminalView {
        let view = TerminalView(frame: .zero)
        view.terminalDelegate = context.coordinator
        context.coordinator.attach(view)
        if !buffered.isEmpty {
            view.feed(byteArray: [UInt8](buffered)[...])
        }
        onReady { data in
            view.feed(byteArray: [UInt8](data)[...])
        }
        return view
    }

    func updateUIView(_ uiView: TerminalView, context: Context) {
        context.coordinator.onSend = onSend
        context.coordinator.onResize = onResize
        uiView.terminalDelegate = context.coordinator
        if focusToken != context.coordinator.lastFocusToken {
            context.coordinator.lastFocusToken = focusToken
            _ = uiView.becomeFirstResponder()
        }
    }

    @MainActor
    final class Coordinator: NSObject, TerminalViewDelegate {
        var onSend: (String) -> Void
        var onResize: (Int, Int) -> Void
        var lastFocusToken: UInt64 = 0
        private var lastCols = 0
        private var lastRows = 0

        init(onSend: @escaping (String) -> Void, onResize: @escaping (Int, Int) -> Void) {
            self.onSend = onSend
            self.onResize = onResize
        }

        func attach(_ view: TerminalView) {
            let cols = view.getTerminal().cols
            let rows = view.getTerminal().rows
            lastCols = cols
            lastRows = rows
            onResize(cols, rows)
        }

        func sizeChanged(source: TerminalView, newCols: Int, newRows: Int) {
            guard newCols != lastCols || newRows != lastRows else { return }
            lastCols = newCols
            lastRows = newRows
            onResize(newCols, newRows)
        }

        func setTerminalTitle(source: TerminalView, title: String) {}

        func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {}

        func send(source: TerminalView, data: ArraySlice<UInt8>) {
            onSend(String(decoding: data, as: UTF8.self))
        }

        func scrolled(source: TerminalView, position: Double) {}

        func requestOpenLink(source: TerminalView, link: String, params: [String: String]) {
            guard let url = URL(string: link) else { return }
            UIApplication.shared.open(url)
        }

        func bell(source: TerminalView) {}

        func clipboardCopy(source: TerminalView, content: Data) {
            UIPasteboard.general.string = String(decoding: content, as: UTF8.self)
        }

        func clipboardRead(source: TerminalView) -> Data? { nil }

        func iTermContent(source: TerminalView, content: ArraySlice<UInt8>) {}

        func rangeChanged(source: TerminalView, startY: Int, endY: Int) {}
    }
}
