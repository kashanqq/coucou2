// node --test — run with `npm test`. Node strips the types itself, no extra deps.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  ATTENTION_MS,
  announceKey,
  dueForAnnouncement,
  dayColumns,
  dueLabel,
  groupTasks,
  needsAttention,
  nextAnnouncementAt,
  type DeadlineTask,
} from "./deadlines.ts";

const NOW = Date.parse("2026-10-15T12:00:00Z");
const MIN = 60_000;
const HOUR = 60 * MIN;

function task(id: string, offsetMs: number): DeadlineTask {
  return { id, title: `Task ${id}`, url: null, deadline: new Date(NOW + offsetMs).toISOString() };
}

test("attention window is two hours", () => {
  assert.equal(ATTENTION_MS, 2 * HOUR);
});

test("groupTasks splits overdue, attention and upcoming", () => {
  const g = groupTasks([
    task("late", -30 * MIN),
    task("soon", 90 * MIN),
    task("later", 5 * HOUR),
  ], NOW);
  assert.deepEqual(g.overdue.map((t) => t.id), ["late"]);
  assert.deepEqual(g.attention.map((t) => t.id), ["soon"]);
  assert.deepEqual(g.upcoming.map((t) => t.id), ["later"]);
});

test("attention means strictly less than two hours left", () => {
  const g = groupTasks([task("edge", 2 * HOUR), task("inside", 2 * HOUR - 1)], NOW);
  assert.deepEqual(g.attention.map((t) => t.id), ["inside"]);
  assert.deepEqual(g.upcoming.map((t) => t.id), ["edge"]);
});

test("needsAttention: anything overdue or under two hours", () => {
  assert.equal(needsAttention([task("later", 5 * HOUR)], NOW), false);
  assert.equal(needsAttention([task("later", 5 * HOUR), task("soon", 30 * MIN)], NOW), true);
  assert.equal(needsAttention([task("late", -MIN)], NOW), true);
  assert.equal(needsAttention([], NOW), false);
});

test("a deadline exactly now is overdue", () => {
  const g = groupTasks([task("now", 0)], NOW);
  assert.deepEqual(g.overdue.map((t) => t.id), ["now"]);
});

test("each group is sorted by deadline; overdue starts with the most recent", () => {
  const g = groupTasks([
    task("u2", 30 * HOUR), task("u1", 3 * HOUR),
    task("a2", 100 * MIN), task("a1", 10 * MIN),
    task("o-old", -3 * 24 * HOUR), task("o-new", -5 * MIN),
  ], NOW);
  assert.deepEqual(g.upcoming.map((t) => t.id), ["u1", "u2"]);
  assert.deepEqual(g.attention.map((t) => t.id), ["a1", "a2"]);
  assert.deepEqual(g.overdue.map((t) => t.id), ["o-new", "o-old"]);
});

test("tasks with an unreadable deadline are left out", () => {
  const g = groupTasks([{ id: "x", title: "x", url: null, deadline: "someday" }], NOW);
  assert.equal(g.overdue.length + g.attention.length + g.upcoming.length, 0);
});

test("dueLabel reads in minutes, hours and days", () => {
  assert.equal(dueLabel(NOW + 40 * MIN, NOW), "in 40 min");
  assert.equal(dueLabel(NOW + 100 * MIN, NOW), "in 1 h 40 min");
  assert.equal(dueLabel(NOW + 2 * HOUR, NOW), "in 2 h");
  assert.equal(dueLabel(NOW + 30 * 1000, NOW), "in 1 min");
  assert.equal(dueLabel(NOW - 25 * MIN, NOW), "25 min late");
  assert.equal(dueLabel(NOW - 3 * HOUR - 5 * MIN, NOW), "3 h 5 min late");
  assert.equal(dueLabel(NOW - 50 * HOUR, NOW), "2 d late");
  assert.equal(dueLabel(NOW + 50 * HOUR, NOW), "in 2 d");
});

test("dueForAnnouncement: in the window, not yet announced, not overdue", () => {
  const tasks = [task("late", -MIN), task("soon", 30 * MIN), task("done", 60 * MIN), task("later", 3 * HOUR)];
  const announced = new Set([announceKey(tasks[2])]);
  assert.deepEqual(dueForAnnouncement(tasks, NOW, announced).map((t) => t.id), ["soon"]);
});

test("a moved deadline is announced again", () => {
  const before = task("t", 30 * MIN);
  const after = task("t", 45 * MIN);
  const announced = new Set([announceKey(before)]);
  assert.deepEqual(dueForAnnouncement([after], NOW, announced).map((t) => t.id), ["t"]);
});

test("nextAnnouncementAt: when the next task enters the window", () => {
  const tasks = [task("a", 5 * HOUR), task("b", 3 * HOUR), task("late", -HOUR)];
  // First moment it is strictly under two hours away.
  assert.equal(nextAnnouncementAt(tasks, NOW, new Set()), NOW + HOUR + 1);
});

test("nextAnnouncementAt: now when a task is already in the window", () => {
  assert.equal(nextAnnouncementAt([task("a", 30 * MIN)], NOW, new Set()), NOW);
});

test("nextAnnouncementAt: null when nothing is left to announce", () => {
  const t = task("a", 30 * MIN);
  assert.equal(nextAnnouncementAt([t, task("late", -MIN)], NOW, new Set([announceKey(t)])), null);
});

// ── Calendar columns (local days) ──────────────────────────────────────────────

const local = (d: number, h = 12, min = 0) => new Date(2026, 9, d, h, min).getTime();
const at = (id: string, ms: number): DeadlineTask =>
  ({ id, title: id, url: null, deadline: new Date(ms).toISOString() });
const NOON_15 = local(15);

test("dayColumns: overdue first, then a week of days from today", () => {
  const cols = dayColumns([at("late", local(14)), at("today", local(15, 18)), at("fri", local(16, 9))], NOON_15);
  assert.deepEqual(cols.map((c) => c.kind), ["overdue", "day", "day", "day", "day", "day", "day", "day"]);
  assert.equal(cols[1].day, local(15, 0));
  assert.equal(cols[2].day, local(16, 0));
  assert.deepEqual(cols.map((c) => c.tasks.map((t) => t.id)), [["late"], ["today"], ["fri"], [], [], [], [], []]);
});

test("dayColumns: no overdue column when nothing is late", () => {
  const cols = dayColumns([at("a", local(15, 18))], NOON_15);
  assert.equal(cols[0].kind, "day");
  assert.equal(cols.length, 7);
});

test("dayColumns: runs on to the last deadline, empty days included", () => {
  const cols = dayColumns([at("far", local(25, 10))], NOON_15);
  assert.equal(cols.length, 11); // the 15th … the 25th
  assert.deepEqual(cols[10].tasks.map((t) => t.id), ["far"]);
  assert.ok(cols.slice(0, 10).every((c) => c.tasks.length === 0));
});

test("dayColumns: a busy day keeps every task, soonest first", () => {
  const ids = ["c", "a", "b"];
  const hours = [17, 9, 13];
  const cols = dayColumns(ids.map((id, i) => at(id, local(16, hours[i]))), NOON_15);
  assert.deepEqual(cols[1].tasks.map((t) => t.id), ["a", "b", "c"]);
});

test("dayColumns: earlier today but already past is overdue, not today", () => {
  const cols = dayColumns([at("morning", local(15, 9))], NOON_15);
  assert.equal(cols[0].kind, "overdue");
  assert.deepEqual(cols[0].tasks.map((t) => t.id), ["morning"]);
});

test("dayColumns: beyond a month goes to one Later column, nothing is dropped", () => {
  const cols = dayColumns([at("soon", local(16)), at("next-year", new Date(2027, 0, 5).getTime())], NOON_15);
  const last = cols[cols.length - 1];
  assert.equal(last.kind, "later");
  assert.deepEqual(last.tasks.map((t) => t.id), ["next-year"]);
  assert.equal(cols.filter((c) => c.kind === "day").length, 7);
});

test("dayColumns: unreadable deadlines are left out", () => {
  const cols = dayColumns([{ id: "x", title: "x", url: null, deadline: "nope" }], NOON_15);
  assert.equal(cols.reduce((n, c) => n + c.tasks.length, 0), 0);
});
