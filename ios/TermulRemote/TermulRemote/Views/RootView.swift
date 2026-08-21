import SwiftUI

struct RootView: View {
    @Bindable var store: ConnectionStore

    var body: some View {
        NavigationStack(path: $store.path) {
            PairingView(store: store)
                .navigationDestination(for: ConnectionStore.Route.self) { route in
                    switch route {
                    case .session(let link):
                        SessionView(link: link)
                    }
                }
        }
        .alert(
            String(localized: "Could not open link"),
            isPresented: Binding(
                get: { store.errorMessage != nil },
                set: { if !$0 { store.dismissError() } }
            )
        ) {
        } message: {
            Text(store.errorMessage ?? "")
        }
    }
}
