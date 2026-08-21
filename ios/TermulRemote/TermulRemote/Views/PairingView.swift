import SwiftUI

struct PairingView: View {
    @Bindable var store: ConnectionStore
    @State private var draftURL = ""
    @State private var scanner: ScannerPayload?

    var body: some View {
        List {
            Section {
                TextField(String(localized: "Paste access link"), text: $draftURL)
                    .textContentType(.URL)
                    .keyboardType(.URL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .submitLabel(.go)
                    .onSubmit { store.connect(to: draftURL) }

                Button(String(localized: "Open link"), systemImage: "arrow.up.right.square") {
                    store.connect(to: draftURL)
                }
                .disabled(draftURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)

                Button(String(localized: "Scan QR code"), systemImage: "qrcode.viewfinder") {
                    scanner = ScannerPayload()
                }
            } footer: {
                Text(
                    String(
                        localized: "Scan the QR from Termul’s desktop status bar, or paste the copied HTTPS link. The access token stays in the URL fragment."
                    )
                )
            }

            Section(String(localized: "Saved connections")) {
                if store.savedLinks.isEmpty {
                    ContentUnavailableView(
                        String(localized: "No saved connections"),
                        systemImage: "iphone.and.arrow.forward",
                        description: Text(String(localized: "Scan or paste a link from the desktop app."))
                    )
                    .listRowBackground(Color.clear)
                } else {
                    ForEach(store.savedLinks) { link in
                        Button {
                            store.connect(link: link)
                        } label: {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(link.title)
                                    .foregroundStyle(.primary)
                                Text(link.originHost)
                                    .font(.footnote)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                    .onDelete { indexSet in
                        for index in indexSet {
                            store.forget(store.savedLinks[index])
                        }
                    }
                }
            }
        }
        .navigationTitle(String(localized: "Termul"))
        .sheet(item: $scanner) { _ in
            ScannerView { scanned in
                scanner = nil
                draftURL = scanned
                store.connect(to: scanned)
            }
        }
    }
}

struct ScannerPayload: Identifiable {
    let id = UUID()
}
