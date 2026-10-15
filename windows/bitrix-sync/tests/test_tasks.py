import json
import tempfile
import unittest
from pathlib import Path

from bitrix_sync.tasks import is_login_url, normalize, portal_origin, write_atomic

PORTAL = "https://umschool.bitrix24.kz"


def raw(id, deadline="2026-10-20T18:00:00+05:00", status="3", responsible="7", title=None):
    return {"id": str(id), "title": title or f"Task {id}", "deadline": deadline,
            "status": status, "responsibleId": responsible}


class NormalizeTest(unittest.TestCase):
    def test_planned_format_with_link_built_from_the_id(self):
        self.assertEqual(normalize([raw(42, title="Отчёт")], PORTAL, 7), [{
            "id": "42",
            "title": "Отчёт",
            "url": f"{PORTAL}/company/personal/user/7/tasks/task/view/42/",
            "deadline": "2026-10-20T18:00:00+05:00",
        }])

    def test_skips_tasks_without_a_deadline(self):
        tasks = [raw(1, deadline=None), raw(2, deadline=""), raw(3)]
        self.assertEqual([t["id"] for t in normalize(tasks, PORTAL, 7)], ["3"])

    def test_skips_deadlines_without_a_time_zone(self):
        tasks = [raw(1, deadline="2026-10-20T18:00:00"), raw(2, deadline="garbage"), raw(3)]
        self.assertEqual([t["id"] for t in normalize(tasks, PORTAL, 7)], ["3"])

    def test_skips_completed_and_declined(self):
        tasks = [raw(1, status="5"), raw(2, status="7"), raw(3, status="4"), raw(4, status="2")]
        self.assertEqual([t["id"] for t in normalize(tasks, PORTAL, 7)], ["3", "4"])

    def test_only_my_own_tasks(self):
        tasks = [raw(1, responsible="8"), raw(2)]
        self.assertEqual([t["id"] for t in normalize(tasks, PORTAL, 7)], ["2"])

    def test_accepts_upper_case_rest_keys(self):
        task = {"ID": "5", "TITLE": "Up", "DEADLINE": "2026-10-20T18:00:00+05:00",
                "STATUS": "3", "RESPONSIBLE_ID": "7"}
        self.assertEqual([t["id"] for t in normalize([task], PORTAL, 7)], ["5"])

    def test_sorted_by_deadline(self):
        tasks = [raw(1, "2026-10-21T10:00:00+05:00"), raw(2, "2026-10-20T10:00:00Z"),
                 raw(3, "2026-10-20T12:00:00+05:00")]
        # 12:00+05:00 is 07:00Z, before 10:00Z.
        self.assertEqual([t["id"] for t in normalize(tasks, PORTAL, 7)], ["3", "2", "1"])

    def test_title_is_trimmed_and_required(self):
        tasks = [raw(1, title="  Hi  "), {**raw(2), "title": "   "}]
        self.assertEqual([t["title"] for t in normalize(tasks, PORTAL, 7)], ["Hi"])


class PortalTest(unittest.TestCase):
    def test_origin_from_any_portal_url(self):
        self.assertEqual(portal_origin("https://umschool.bitrix24.kz/"), PORTAL)
        self.assertEqual(portal_origin("umschool.bitrix24.kz"), PORTAL)
        self.assertEqual(portal_origin(" https://umschool.bitrix24.kz/stream/ "), PORTAL)

    def test_rejects_non_https(self):
        with self.assertRaises(ValueError):
            portal_origin("http://umschool.bitrix24.kz")
        with self.assertRaises(ValueError):
            portal_origin("")

    def test_login_pages(self):
        self.assertFalse(is_login_url(f"{PORTAL}/stream/", PORTAL))
        self.assertTrue(is_login_url(f"{PORTAL}/auth/?backurl=%2Fstream%2F", PORTAL))
        self.assertTrue(is_login_url("https://www.bitrix24.net/oauth/authorize/?x=1", PORTAL))
        # Bitrix keeps ?login=yes in the address after a successful sign-in.
        self.assertFalse(is_login_url(f"{PORTAL}/?login=yes", PORTAL))


class WriteAtomicTest(unittest.TestCase):
    def test_writes_utf8_json_and_creates_the_folder(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "nested" / "tasks.json"
            write_atomic(out, [{"title": "Отчёт"}])
            self.assertEqual(json.loads(out.read_text(encoding="utf-8")), [{"title": "Отчёт"}])
            self.assertEqual([p.name for p in out.parent.iterdir()], ["tasks.json"])

    def test_replaces_an_existing_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "tasks.json"
            out.write_text("old", encoding="utf-8")
            write_atomic(out, [])
            self.assertEqual(json.loads(out.read_text(encoding="utf-8")), [])


if __name__ == "__main__":
    unittest.main()
