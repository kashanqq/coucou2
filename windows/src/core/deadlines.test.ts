// node --test — run with `npm test`. Node strips the types itself, no extra deps.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  ATTENTION_MS,
  announceKey,
  dueForAnnouncement,
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
