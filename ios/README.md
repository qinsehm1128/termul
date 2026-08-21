# Termul iOS companion

Native SwiftUI client for the desktop shared-live session. It does **not** run a PTY or tunnel sidecar. The desktop app still hosts the session; this app scans or pastes the HTTPS access URL and opens the existing mobile web client inside `WebView`.

## Open in Xcode

1. Open `ios/TermulRemote/TermulRemote.xcodeproj`.
2. Select your Development Team for signing (`com.termul.remote`).
3. Run on a physical iPhone to use the camera QR scanner. Simulator can paste the copied link.

Requires Xcode 26 and iOS 26.

## Pairing

1. On the desktop, enable remote access in the status bar.
2. Scan the QR, or paste the copied `https://…/#access_token=…` link.
3. The token stays in the URL fragment and is consumed by the existing web client.

Deep link: `termul://open?url=<encoded-https-access-url>`.

HTTP FRP origins are rejected. Put TLS in front of FRP, or use Cloudflare.
