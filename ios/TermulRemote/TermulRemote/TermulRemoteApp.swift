import SwiftUI

@main
struct TermulRemoteApp: App {
    @State private var store = ConnectionStore()

    var body: some Scene {
        WindowGroup {
            RootView(store: store)
                .onOpenURL { store.openIncomingURL($0) }
        }
    }
}
