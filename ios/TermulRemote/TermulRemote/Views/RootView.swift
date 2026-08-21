import SwiftUI

struct RootView: View {
    @Bindable var store: ConnectionStore
    @Bindable var settings: AppSettings

    var body: some View {
        Group {
            if let link = store.activeLink {
                WorkspaceView(store: store, link: link)
                    .transition(.opacity.combined(with: .move(edge: .trailing)))
            } else {
                HomeView(store: store, settings: settings)
                    .transition(.opacity)
            }
        }
        .animation(.snappy(duration: 0.28), value: store.activeLink?.id)
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
