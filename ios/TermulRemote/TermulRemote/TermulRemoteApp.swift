import SwiftUI

@main
struct TermulRemoteApp: App {
    @State private var store = ConnectionStore()
    @State private var settings = AppSettings()

    var body: some Scene {
        WindowGroup {
            RootView(store: store, settings: settings)
                .environment(\.locale, settings.locale)
                .preferredColorScheme(settings.appearance.colorScheme)
                .tint(TermulTheme.accent)
                .onOpenURL { store.openIncomingURL($0) }
        }
    }
}
