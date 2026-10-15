"""Pure helpers: Bitrix24 REST tasks → Coucou's tasks.json. No browser here."""

import json
import os
import tempfile
from datetime import datetime
from pathlib import Path
from urllib.parse import urlsplit

# Bitrix24 task statuses that are finished: 5 = completed, 7 = declined (legacy).
DONE_STATUSES = {"5", "7"}


def _field(task, name):
    """tasks.task.list answers in camelCase; older select lists in UPPER_CASE."""
    upper = "".join("_" + c if c.isupper() else c for c in name).upper()
    value = task.get(name, task.get(upper))
    return "" if value is None else str(value).strip()


def _deadline(value):
    """The deadline as a time-zoned datetime, or None if it isn't one."""
    try:
        parsed = datetime.fromisoformat(value)
    except ValueError:
        return None
    return parsed if parsed.tzinfo is not None else None


def normalize(raw, portal, user_id):
    """Open tasks where the user is responsible and a deadline is set, soonest first."""
    out = []
    for task in raw:
        task_id = _field(task, "id")
        title = _field(task, "title")
        deadline = _field(task, "deadline")
        at = _deadline(deadline)
        if not task_id or not title or at is None:
            continue
        if _field(task, "status") in DONE_STATUSES:
            continue
        if _field(task, "responsibleId") != str(user_id):
            continue
        out.append((at, {
            "id": task_id,
            "title": title,
            "url": f"{portal}/company/personal/user/{user_id}/tasks/task/view/{task_id}/",
            "deadline": deadline,
        }))
    out.sort(key=lambda pair: pair[0])
    return [task for _, task in out]


def portal_origin(url):
    """https://host from whatever the user typed: a bare host, a page URL, a trailing slash."""
    url = url.strip()
    if url and "://" not in url:
        url = "https://" + url
    parts = urlsplit(url)
    if parts.scheme != "https" or not parts.hostname:
        raise ValueError(f"Not an https Bitrix24 address: {url!r}")
    return f"https://{parts.netloc}"


def is_login_url(url, portal):
    """True when the portal sent the browser to a sign-in page.

    Not `?login=yes`: Bitrix keeps that in the address after a successful
    sign-in. Whether someone is signed in is the page's own USER_ID to tell.
    """
    parts = urlsplit(url)
    if f"https://{parts.netloc}" != portal:
        return True  # bitrix24.net OAuth and friends
    return parts.path.startswith("/auth")


def write_atomic(path, data):
    """Write next to the target, then swap it in: Coucou never reads half a file."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix=".tasks-", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            json.dump(data, f, ensure_ascii=False, indent=2)
        os.replace(tmp, path)
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise
