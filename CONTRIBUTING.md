# Contributing to Coucou

Thanks for wanting to help Mochi grow up! 🫶

## Getting started

**macOS** (`NotchBuddy/`):

```bash
brew install xcodegen
cd NotchBuddy && xcodegen && open NotchBuddy.xcodeproj
```

Never edit `NotchBuddy.xcodeproj` by hand: change `project.yml` and run `xcodegen`.

**Windows** (`windows/`): needs [Rust](https://rustup.rs), Node 20+ and the MSVC build tools.

```powershell
cd windows
npm install
npm run tauri dev   # live-reloading development build
npm run dev         # front end only, in a browser
```

See [`windows/README.md`](windows/README.md) for the layout and packaging.

## Good first contributions

- A new integration (a poller + a pill + a detail card). Look at `StripePoller.swift` (macOS) or `windows/src-tauri/src/integrations.rs` + `windows/src/views/integrations.ts` (Windows).
- A new emote or sound for Mochi.
- Bug fixes — please describe how to reproduce, and say which platform.

## Rules of the house

- **No third-party dependencies** unless there's really no other way. macOS: Swift 6, SwiftUI + AppKit. Windows: Tauri 2, Rust + TypeScript without a UI framework.
- Secrets go in the macOS Keychain or the Windows Credential Manager, never on disk or in git.
- No telemetry, no network calls except to services the user configured.
- Never block Claude Code: if the app doesn't answer, the hook must exit right away.
- Never write `~/.claude/settings.json` without a backup and the user's confirmation.
- Keep it light: 0 % CPU when the island is hidden.
- Behaviour follows [`docs/SPEC.md`](docs/SPEC.md) on both platforms; visuals follow `design/prototype/` and `design/captures/`.

## Pull requests

- One topic per PR, with a short GIF or screenshot for anything visual.
- Build must pass with no new warnings (`xcodebuild` on macOS, `npm run build` + `cargo build` in `windows/` on Windows).
