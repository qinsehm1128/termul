import SwiftUI
import VisionKit

struct ScannerView: View {
    var onScan: (String) -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Group {
                if DataScannerViewController.isSupported, DataScannerViewController.isAvailable {
                    DataScannerRepresentable(onScan: onScan)
                } else {
                    ContentUnavailableView(
                        String(localized: "Camera unavailable"),
                        systemImage: "qrcode.viewfinder",
                        description: Text(String(localized: "Use a physical iPhone, or paste the link instead."))
                    )
                }
            }
            .navigationTitle(String(localized: "Scan QR"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(String(localized: "Close"), systemImage: "xmark") {
                        dismiss()
                    }
                }
            }
        }
    }
}

private struct DataScannerRepresentable: UIViewControllerRepresentable {
    var onScan: (String) -> Void

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let scanner = DataScannerViewController(
            recognizedDataTypes: [.barcode(symbologies: [.qr])],
            qualityLevel: .balanced,
            recognizesMultipleItems: false,
            isHighFrameRateTrackingEnabled: false,
            isPinchToZoomEnabled: true,
            isGuidanceEnabled: true,
            isHighlightingEnabled: true
        )
        scanner.delegate = context.coordinator
        context.coordinator.controller = scanner
        try? scanner.startScanning()
        return scanner
    }

    func updateUIViewController(_: DataScannerViewController, context _: Context) {}

    static func dismantleUIViewController(_ controller: DataScannerViewController, coordinator _: Coordinator) {
        controller.stopScanning()
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(onScan: onScan)
    }

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        var onScan: (String) -> Void
        weak var controller: DataScannerViewController?

        init(onScan: @escaping (String) -> Void) {
            self.onScan = onScan
        }

        func dataScanner(
            _: DataScannerViewController,
            didTapOn item: RecognizedItem
        ) {
            if case let .barcode(barcode) = item, let payload = barcode.payloadStringValue {
                controller?.stopScanning()
                onScan(payload)
            }
        }

        func dataScanner(
            _: DataScannerViewController,
            didAdd addedItems: [RecognizedItem],
            allItems _: [RecognizedItem]
        ) {
            guard let item = addedItems.first else { return }
            if case let .barcode(barcode) = item, let payload = barcode.payloadStringValue {
                controller?.stopScanning()
                onScan(payload)
            }
        }
    }
}
