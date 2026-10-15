"""Command line — the whole contract with Coucou.

    python -m bitrix_sync login --portal URL --profile DIR
    python -m bitrix_sync sync  --portal URL --profile DIR --out tasks.json

The last stdout line is one JSON object, and the exit code says the same:
  0 {"status": "ok", ...}   1 {"status": "error", "message": ...}
  3 {"status": "needs-login"}   (2 is argparse's own usage error)
"""

import argparse
import json
import sys

from . import NeedsLogin
from .tasks import normalize, portal_origin, write_atomic

EXIT_OK = 0
EXIT_ERROR = 1
EXIT_NEEDS_LOGIN = 3

__all__ = ["EXIT_ERROR", "EXIT_NEEDS_LOGIN", "EXIT_OK", "NeedsLogin", "main"]


def _parser():
    parser = argparse.ArgumentParser(prog="bitrix_sync", description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    for name, help in (("login", "open a visible browser and sign in"),
                       ("sync", "write the open tasks with a deadline to --out")):
        cmd = sub.add_parser(name, help=help)
        cmd.add_argument("--portal", required=True, help="e.g. https://umschool.bitrix24.kz")
        cmd.add_argument("--profile", required=True, help="browser profile folder (keeps the session)")
        if name == "sync":
            cmd.add_argument("--out", required=True, help="tasks.json to write")
    return parser


def main(argv=None, fetch=None, login=None, stdout=None):
    stdout = stdout or sys.stdout
    args = _parser().parse_args(argv)

    def report(code, **status):
        print(json.dumps(status, ensure_ascii=False), file=stdout, flush=True)
        return code

    try:
        portal = portal_origin(args.portal)
        if args.command == "login":
            if login is None:
                from .browser import login
            login(portal, args.profile)
            return report(EXIT_OK, status="ok")

        if fetch is None:
            from .browser import fetch_tasks as fetch
        user_id, raw = fetch(portal, args.profile)
        tasks = normalize(raw, portal, user_id)
        write_atomic(args.out, tasks)
        return report(EXIT_OK, status="ok", count=len(tasks), out=args.out)
    except NeedsLogin:
        return report(EXIT_NEEDS_LOGIN, status="needs-login")
    except Exception as err:  # noqa: BLE001 — every failure becomes a status line
        return report(EXIT_ERROR, status="error", message=str(err) or type(err).__name__)
