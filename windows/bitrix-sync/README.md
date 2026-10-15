# Bitrix24 sync service

A small Python program that writes your open Bitrix24 tasks to `tasks.json`
for Coucou's Bitrix24 tab. It is a separate process, not part of the app:
Coucou starts it now and then and reads back its exit code and one JSON line.
The tasks travel through the file.

```
coucou.exe ──every ~45 min──▶ python -m bitrix_sync sync ──▶ tasks.json ──▶ coucou.exe
             (Settings: Sign in…) python -m bitrix_sync login  (visible Edge window)
```

## Why it works this way

The portal gives ordinary employees no webhook or REST app, so the service
works as you in a browser: Playwright drives the **Microsoft Edge that ships
with Windows**, with a profile of its own (`%LOCALAPPDATA%\Coucou\bitrix-profile`).
You sign in once in a visible window — password and 2FA typed by you — and
Edge keeps the session, encrypted with your Windows account. The service never
sees or stores your password.

Each sync loads one portal page and asks the same REST endpoint the portal's own
pages use, with your session: tasks where you are responsible, not completed,
with a deadline. Read-only, your tasks only, about every 45 minutes (± 10), never
two at once, nothing while Coucou is paused.

Light on the PC: the headless Edge doesn't load images, and every run ends with
no process left over. Coucou puts the service in a Windows job object, so
Python, Playwright's driver and Edge all go when it finishes, times out (3 min)
or Coucou quits. No Edge starts while the portal is unreachable (Coucou checks
with a single HEAD request, once a minute while a sync is due) or while the
session needs a sign-in.

The next run is a time on the wall clock, kept in
`%LOCALAPPDATA%\Coucou\bitrix-sync.json`, not a countdown. Sleep, a shutdown
or time offline count towards it, so a sync that came due meanwhile runs within
a minute of waking up or of the network coming back.

## Setup

```powershell
.\setup.ps1          # creates .venv and installs Playwright (no browser download)
```

Then in Coucou: **Settings → Bitrix24**, enter the portal
(`https://umschool.bitrix24.kz`), **Save**, then **Sign in…**.

## By hand

```powershell
.\.venv\Scripts\python -m bitrix_sync login --portal https://umschool.bitrix24.kz --profile "$env:LOCALAPPDATA\Coucou\bitrix-profile"
.\.venv\Scripts\python -m bitrix_sync sync  --portal https://umschool.bitrix24.kz --profile "$env:LOCALAPPDATA\Coucou\bitrix-profile" --out tasks.json
```

| Exit | Last line | Meaning |
|---|---|---|
| 0 | `{"status": "ok", "count": 4, "out": "…"}` | `tasks.json` written (atomically) |
| 3 | `{"status": "needs-login"}` | the session is gone: run `login` |
| 1 | `{"status": "error", "message": "…"}` | anything else; the file is left as it was |

`tasks.json` is an array of `{"id", "title", "url", "deadline"}`, soonest first,
deadlines in RFC 3339 with their time zone.

## Tests

```powershell
.\.venv\Scripts\python -m unittest
```

The tests cover everything except the browser itself (`browser.py`).
