# Coucou — guide for AI coding agents

Coucou puts Mochi, a small animated character, at the top of the screen: it shows Claude Code sessions and a few integrations, and lets the user approve, answer, chat and drop files from the island. Two apps share one repo:

- **macOS** (`NotchBuddy/`): native Swift app living in the MacBook notch.
- **Windows** (`windows/`): Tauri 2 app (Rust backend + TypeScript front end). No notch, so the island sits at the top centre of the screen. This is the current focus.

## Where things are

Shared:
- `docs/SPEC.md`, `docs/INTEGRATIONS.md` — behaviour, views, states, integrations (in French). Both apps follow them.
- `design/prototype/notch-buddy.html` — original prototype, the visual source of truth. `design/captures/` — target screenshots.
- `NotchBuddy/Resources/sounds/` — the 28 WAV sounds, used by both apps (Windows reads them via `SOUNDS_DIR` in `windows/vite.config.ts`; never copy them).
- `docs/*.html` — the GitHub Pages site (privacy, terms, support, legal notice).
- `.github/workflows/` — `build.yml`, `release.yml` (macOS); `windows.yml` (Windows, triggered by `windows-v*` tags).

macOS:
- `NotchBuddy/Sources/App/` — all Swift code. `NotchBuddy/project.yml` — XcodeGen project (never edit the `.xcodeproj` by hand).

Windows (`windows/`, see `windows/README.md`):
- `src/` — island front end, TypeScript, no framework: `mochi/` (Mochi in Canvas 2D), `island/` (state machine, hooks, integrations), `views/`, `settings/`, `core/`, `upload/`.
- `src-tauri/src/` — Rust backend: window, named pipe (`pipe.rs`), Claude API, pollers, secrets, hooks install.
- `hook/` — `coucou-hook.exe`, the Claude Code relay (Cargo workspace member).
- `scripts/` — icon generator (`gen-icons.mjs`) and installer packaging (`pack.mjs`). `dev/` — browser-only previews, never shipped.

## Build

macOS:
```
cd NotchBuddy && xcodegen && xcodebuild -scheme NotchBuddy -configuration Debug build
```

Windows (Rust, Node 20+, MSVC build tools):
```
cd windows
npm install
npm run tauri dev   # dev build with live reload
npm run dev         # front end only, in a browser
npm run build       # tsc --noEmit + vite build
npm test            # node --test on src/**/*.test.ts (Node 22.18+)
cargo test          # Rust unit tests
npm run pack        # installer into windows/release/
npm run icons       # regenerate src-tauri/icons
```

## Rules (both apps)
- No third-party dependencies unless truly unavoidable. Mochi is drawn in code, no Rive/Lottie/images.
- Secrets live in the OS credential store (macOS Keychain, Windows Credential Manager), never on disk or in git. The UI can only ask whether a key exists.
- No telemetry. Network calls only to services the user configured.
- Never block Claude Code: if the app doesn't answer, the hook exits immediately (Windows: 300 ms budget, then exit 0).
- Never overwrite `~/.claude/settings.json` (`%USERPROFILE%\.claude\settings.json` on Windows): dated backup, merge, show the diff, write only after the user confirms. Uninstall removes only Coucou's entries.
- Never send an email or approve a Claude Code permission without an explicit click.
- Performance: 0 % CPU when the island is hidden.
- Visual changes must match the prototype and the screenshots in `design/captures/`.
- Keep the two apps' behaviour in step with `docs/SPEC.md`; document deliberate differences in `windows/README.md` ("What's different from the Mac version").

## macOS-specific
- Swift 6, SwiftUI + AppKit. Mochi uses `Canvas` + `TimelineView`.
- Keep the bundle identifier `fr.louisraille.NotchBuddy` (Keychain items, preferences and permissions depend on it).

## Windows-specific
- Tauri 2, Rust (edition 2021) + TypeScript with no UI framework. Mochi uses Canvas 2D.
- Keep the identifier `fr.louisraille.coucou` (`tauri.conf.json` and the Credential Manager service name in `secrets.rs` depend on it).
- Hook ↔ app over the named pipe `\\.\pipe\coucou-<sid>`; the name built by `pipe_path()` in `hook/src/main.rs` and in `src-tauri/src/pipe.rs` must stay identical.
- The hook must start fast: keep `coucou-hook` small, and keep the shared release profile in `windows/Cargo.toml` (size-optimised, LTO, `panic = "abort"`).
- Log: `%LOCALAPPDATA%\Coucou\coucou.log`, local only.

## Agent skills

### Issue tracker

GitHub Issues on `kashanqq/coucou2` via `gh` (always pass `--repo kashanqq/coucou2`). See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one root `CONTEXT.md` + `docs/adr/`. See `docs/agents/domain.md`.
