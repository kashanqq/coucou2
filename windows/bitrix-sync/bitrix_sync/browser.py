"""The only part that drives a browser: the installed Microsoft Edge, through
Playwright, with a profile of its own so the Bitrix24 session survives between
runs (Edge encrypts its cookies with the Windows account).

Read-only: one page load, then the same REST endpoint the portal's own pages
call, authenticated by the session — no webhook, no app, nothing to set up on
the portal. Only tasks where the signed-in user is responsible are asked for.
"""

import random
import time

from playwright.sync_api import Error as PlaywrightError
from playwright.sync_api import sync_playwright

from . import NeedsLogin
from .tasks import is_login_url

PAGE_TIMEOUT_MS = 45_000
LOGIN_TIMEOUT_S = 10 * 60
# REST answers that mean the session is gone, not that something broke.
AUTH_ERRORS = {"NO_AUTH_FOUND", "INVALID_CREDENTIALS", "expired_token", "invalid_token",
               "authorization_error", "ACCESS_DENIED"}

# Runs inside the portal page. Pages of 50, a human-ish pause between them.
FETCH_JS = """
async () => {
  const uid = Number((window.BX && BX.message && BX.message('USER_ID')) || 0);
  if (!uid || !BX.bitrix_sessid) return { login: true };
  const tasks = [];
  let start = 0;
  for (let page = 0; page < 20; page++) {
    const body = new URLSearchParams();
    body.set('sessid', BX.bitrix_sessid());
    body.set('filter[RESPONSIBLE_ID]', String(uid));
    body.set('filter[!REAL_STATUS]', '5');
    for (const f of ['ID', 'TITLE', 'DEADLINE', 'STATUS', 'RESPONSIBLE_ID']) body.append('select[]', f);
    body.set('start', String(start));
    const res = await fetch('/rest/tasks.task.list.json', { method: 'POST', body, credentials: 'same-origin' });
    let json;
    try { json = await res.json(); } catch { return { error: 'HTTP ' + res.status }; }
    if (json.error) return { error: json.error_description || json.error, code: json.error };
    tasks.push(...((json.result && json.result.tasks) || []));
    if (json.next == null) break;
    start = json.next;
    await new Promise((r) => setTimeout(r, 600 + Math.random() * 900));
  }
  return { uid, tasks };
}
"""


def _launch(p, profile, headless):
    ctx = p.chromium.launch_persistent_context(
        profile,
        channel="msedge",
        headless=headless,
        no_viewport=not headless,
        viewport={"width": 1440, "height": 900} if headless else None,
    )
    ctx.set_default_timeout(PAGE_TIMEOUT_MS)
    return ctx


def _page(ctx):
    return ctx.pages[0] if ctx.pages else ctx.new_page()


def fetch_tasks(portal, profile):
    """(user id, raw REST tasks) for the signed-in user. Raises NeedsLogin."""
    with sync_playwright() as p:
        ctx = _launch(p, profile, headless=True)
        try:
            page = _page(ctx)
            # Headless Edge says "HeadlessChrome"; send what the visible one sends.
            ua = page.evaluate("navigator.userAgent")
            if "Headless" in ua:
                ctx.set_extra_http_headers({"User-Agent": ua.replace("HeadlessChrome", "Chrome")})

            page.goto(portal + "/stream/", wait_until="domcontentloaded")
            if is_login_url(page.url, portal):
                raise NeedsLogin()
            try:
                page.wait_for_function("() => window.BX && BX.message && BX.bitrix_sessid")
            except PlaywrightError:
                if is_login_url(page.url, portal):
                    raise NeedsLogin() from None
                raise RuntimeError("The Bitrix24 page didn't finish loading.") from None
            page.wait_for_timeout(random.uniform(1500, 3500))

            result = page.evaluate(FETCH_JS)
            if result.get("login"):
                raise NeedsLogin()
            if "error" in result:
                if result.get("code") in AUTH_ERRORS:
                    raise NeedsLogin()
                raise RuntimeError(f"Bitrix24 said: {result['error']}")
            return result["uid"], result["tasks"]
        finally:
            ctx.close()


def _signed_in(page, portal):
    """A portal page that knows who you are. Any tab will do (2FA may open more)."""
    try:
        return not is_login_url(page.url, portal) and bool(page.evaluate(
            "() => Number((window.BX && BX.message && BX.message('USER_ID')) || 0)"
        ))
    except PlaywrightError:
        return False  # mid-navigation, or the window is closing


def login(portal, profile):
    """A visible Edge window on the portal; returns once the user is signed in.

    The user types the password and any 2FA code themselves — nothing is stored
    here but the browser's own session. Closing the window gives up (NeedsLogin).
    """
    with sync_playwright() as p:
        ctx = _launch(p, profile, headless=False)
        closed = []
        ctx.on("close", lambda _: closed.append(True))
        try:
            page = _page(ctx)
            page.goto(portal + "/", wait_until="domcontentloaded")
            deadline = time.monotonic() + LOGIN_TIMEOUT_S
            while time.monotonic() < deadline and not closed:
                if any(_signed_in(tab, portal) for tab in list(ctx.pages)):
                    return
                time.sleep(1)
            raise NeedsLogin()
        finally:
            if not closed:
                ctx.close()
