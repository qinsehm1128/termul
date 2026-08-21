import SwiftUI

struct HomeView: View {
    @Bindable var store: ConnectionStore
    @Bindable var settings: AppSettings
    @State private var draftURL = ""
    @State private var showSettings = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                header
                composer
                recents
            }
            .padding(.horizontal, 20)
            .padding(.top, 28)
            .padding(.bottom, 40)
            .frame(maxWidth: 720)
            .frame(maxWidth: .infinity)
        }
        .background(TermulTheme.canvas.ignoresSafeArea())
        .safeAreaInset(edge: .top, spacing: 0) {
            toolbar
        }
        .sheet(isPresented: $store.isScanning) {
            ScannerView { scanned in
                store.isScanning = false
                draftURL = scanned
                store.connect(to: scanned)
            }
        }
        .sheet(isPresented: $showSettings) {
            SettingsView(settings: settings)
        }
    }

    private var toolbar: some View {
        HStack {
            Text("Termul")
                .font(.system(.headline, design: .serif))
            Spacer()
            Button {
                showSettings = true
            } label: {
                Image(systemName: "gearshape")
                    .font(.body)
                    .frame(minWidth: 44, minHeight: 44)
            }
            .accessibilityLabel(Text("Settings"))
        }
        .padding(.horizontal, 12)
        .background(TermulTheme.canvas.opacity(0.92))
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("What should we work on?")
                .font(TermulTheme.wordmark)
                .foregroundStyle(.primary)
                .fixedSize(horizontal: false, vertical: true)
            Text("Scan the desktop QR, or paste the HTTPS link. Chat and the live terminal stay on the host.")
                .font(.body)
                .foregroundStyle(.secondary)
        }
    }

    private var composer: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .center, spacing: 10) {
                TextField("Paste access link", text: $draftURL, axis: .vertical)
                    .textContentType(.URL)
                    .keyboardType(.URL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .lineLimit(1 ... 3)
                    .submitLabel(.go)
                    .onSubmit { store.connect(to: draftURL) }
                    .frame(minHeight: 44)

                Button {
                    store.isScanning = true
                } label: {
                    Image(systemName: "qrcode.viewfinder")
                        .font(.title3)
                        .frame(minWidth: 44, minHeight: 44)
                }
                .accessibilityLabel(Text("Scan QR code"))
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            .background(TermulTheme.surface)
            .clipShape(RoundedRectangle(cornerRadius: TermulTheme.radius, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: TermulTheme.radius, style: .continuous)
                    .stroke(TermulTheme.stroke, lineWidth: 1)
            )

            Button {
                store.connect(to: draftURL)
            } label: {
                Text("Open session")
                    .font(.body.bold())
                    .frame(maxWidth: .infinity)
                    .frame(minHeight: 48)
            }
            .buttonStyle(.borderedProminent)
            .tint(TermulTheme.accent)
            .disabled(draftURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }

    @ViewBuilder
    private var recents: some View {
        if store.savedLinks.isEmpty {
            ContentUnavailableView(
                "No saved desks",
                systemImage: "laptopcomputer.and.iphone",
                description: Text("A scanned or pasted link stays here for the next open.")
            )
            .frame(maxWidth: .infinity)
            .padding(.top, 12)
        } else {
            VStack(alignment: .leading, spacing: 12) {
                Text("Recents")
                    .font(.title3.bold())
                ForEach(store.savedLinks) { link in
                    ConnectionCard(link: link) {
                        store.connect(link: link)
                    } onDelete: {
                        store.forget(link)
                    }
                }
            }
        }
    }
}

private struct ConnectionCard: View {
    let link: RemoteLink
    var onOpen: () -> Void
    var onDelete: () -> Void

    var body: some View {
        Button(action: onOpen) {
            HStack(spacing: 14) {
                Image(systemName: "dot.radiowaves.left.and.right")
                    .font(.title3)
                    .foregroundStyle(TermulTheme.accent)
                    .frame(width: 44, height: 44)
                    .background(TermulTheme.accent.opacity(0.12))
                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                VStack(alignment: .leading, spacing: 4) {
                    Text(link.title)
                        .font(.body.bold())
                        .foregroundStyle(.primary)
                    Text(link.originHost)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
                Image(systemName: "arrow.up.right")
                    .foregroundStyle(.tertiary)
            }
            .padding(14)
            .background(TermulTheme.surface)
            .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 18, style: .continuous)
                    .stroke(TermulTheme.stroke, lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
        .contextMenu {
            Button("Open chat") { onOpen() }
            Button("Remove", role: .destructive) { onDelete() }
        }
    }
}
