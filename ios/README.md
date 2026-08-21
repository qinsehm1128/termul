# Termul iOS companion

Native SwiftUI client for the desktop shared-live session. It does **not** run a PTY or tunnel sidecar. The desktop still hosts agents and terminals; this app pairs over HTTPS and speaks the same `/ws`, `/terminal/ws`, `/projects`, and `/fs/*` contracts as the browser client.

The pairing chrome is native (home, language, appearance). After you connect, **Chat**, **Terminal**, project switching, and a read-only file tree are native too. There is no WebView.

## Open in Xcode

1. Open `ios/TermulRemote/TermulRemote.xcodeproj`.
2. Select your Development Team for signing (`com.termul.remote`).
3. Run on a physical iPhone to use the camera QR scanner. Simulator can paste the copied link.

Requires Xcode 26 and iOS 26.

## Pairing

1. On the desktop, enable remote access in the status bar.
2. Scan the QR, or paste the copied `https://…` link.
3. Use **Chat** / **Terminal** in the session header. Projects and Files are in the header actions.

Deep link: `termul://open?url=<encoded-https-access-url>`.

HTTP FRP origins are rejected. Put TLS in front of FRP, or use Cloudflare.

Terminal lists the host’s **already running** PTYs for the active project (`list` + `watch`) and shows their scrollback. “New terminal” is optional. Phone resize does not change a desktop-owned PTY. The emulator is [SwiftTerm](https://github.com/migueldeicaza/SwiftTerm) (`ios/Vendor/SwiftTerm`, plugin stripped so Xcode 27 can compile it).

## Language

Settings → Language: system, English, or Simplified Chinese.
