import SwiftUI
import UIKit

/// Claude / Codex-adjacent tokens: warm ink, one terracotta accent, quiet chrome.
enum TermulTheme {
    static let canvas = Color(
        light: Color(red: 0.97, green: 0.96, blue: 0.94),
        dark: Color(red: 0.09, green: 0.09, blue: 0.085)
    )
    static let surface = Color(
        light: Color(red: 1, green: 0.99, blue: 0.98),
        dark: Color(red: 0.145, green: 0.138, blue: 0.128)
    )
    static let stroke = Color(
        light: Color.black.opacity(0.08),
        dark: Color.white.opacity(0.08)
    )
    static let accent = Color(red: 0.86, green: 0.42, blue: 0.30)
    static let wordmark: Font = .system(.largeTitle, design: .serif).bold()
    static let display: Font = .system(.title2, design: .serif)
    static let radius: CGFloat = 22
}

extension Color {
    init(light: Color, dark: Color) {
        self.init(uiColor: UIColor { traits in
            traits.userInterfaceStyle == .dark
                ? UIColor(dark)
                : UIColor(light)
        })
    }
}
