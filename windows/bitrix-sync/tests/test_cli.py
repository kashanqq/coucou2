import io
import json
import tempfile
import unittest
from pathlib import Path

from bitrix_sync.cli import EXIT_ERROR, EXIT_NEEDS_LOGIN, EXIT_OK, NeedsLogin, main

PORTAL = "https://umschool.bitrix24.kz"
TASK = {"id": "1", "title": "A", "deadline": "2026-10-20T18:00:00+05:00",
        "status": "3", "responsibleId": "7"}


def run(argv, fetch=None, login=None):
    out = io.StringIO()
    code = main(argv, fetch=fetch, login=login, stdout=out)
    lines = out.getvalue().strip().splitlines()
    return code, json.loads(lines[-1])


class SyncTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.out = Path(self.tmp.name) / "tasks.json"
        self.argv = ["sync", "--portal", "umschool.bitrix24.kz/", "--profile", self.tmp.name,
                     "--out", str(self.out)]

    def tearDown(self):
        self.tmp.cleanup()

    def test_ok_writes_the_file_and_reports_the_count(self):
        seen = {}

        def fetch(portal, profile):
            seen["portal"], seen["profile"] = portal, profile
            return 7, [TASK]

        code, status = run(self.argv, fetch=fetch)
        self.assertEqual(code, EXIT_OK)
        self.assertEqual(status, {"status": "ok", "count": 1, "out": str(self.out)})
        self.assertEqual(seen, {"portal": PORTAL, "profile": self.tmp.name})
        self.assertEqual(json.loads(self.out.read_text(encoding="utf-8"))[0]["id"], "1")

    def test_needs_login_leaves_the_file_alone(self):
        self.out.write_text("[]", encoding="utf-8")

        def fetch(portal, profile):
            raise NeedsLogin()

        code, status = run(self.argv, fetch=fetch)
        self.assertEqual(code, EXIT_NEEDS_LOGIN)
        self.assertEqual(status["status"], "needs-login")
        self.assertEqual(self.out.read_text(encoding="utf-8"), "[]")

    def test_errors_are_reported_not_raised(self):
        def fetch(portal, profile):
            raise RuntimeError("portal said no")

        code, status = run(self.argv, fetch=fetch)
        self.assertEqual(code, EXIT_ERROR)
        self.assertEqual(status, {"status": "error", "message": "portal said no"})
        self.assertFalse(self.out.exists())

    def test_bad_portal_is_an_error(self):
        argv = ["sync", "--portal", "http://x", "--profile", self.tmp.name, "--out", str(self.out)]
        code, status = run(argv, fetch=lambda p, q: (7, []))
        self.assertEqual(code, EXIT_ERROR)
        self.assertEqual(status["status"], "error")


class LoginTest(unittest.TestCase):
    def test_login_ok(self):
        code, status = run(["login", "--portal", PORTAL, "--profile", "p"], login=lambda portal, profile: None)
        self.assertEqual((code, status), (EXIT_OK, {"status": "ok"}))

    def test_login_abandoned(self):
        def login(portal, profile):
            raise NeedsLogin()

        code, status = run(["login", "--portal", PORTAL, "--profile", "p"], login=login)
        self.assertEqual((code, status["status"]), (EXIT_NEEDS_LOGIN, "needs-login"))


if __name__ == "__main__":
    unittest.main()
