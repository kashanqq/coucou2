// Bitrix24 tasks → island state, plus the deadline heads-up: once a task is
// under two hours from its deadline the island opens on the Bitrix tab, once
// per task. One timer for the next task only — nothing ticks in between.

import { Bridge, onEvent, type BitrixUpdate } from "../core/bridge";
import { Sound } from "../core/sound";
import { State } from "../core/state";
import { announceKey, deadlineMs, dueForAnnouncement, dueLabel, nextAnnouncementAt } from "../core/deadlines";
import type { Island } from "./island";

/** Bitrix24's brand cyan, for the bar note and the tab. */
export const BITRIX_COLOR = "#2FC6F6";

/** setTimeout overflows past ~24.8 days and fires at once; wake up and re-plan instead. */
const MAX_WAIT_MS = 6 * 60 * 60_000;

let reminder: number | null = null;
/** announceKey of every task already announced, so a re-read can't repeat one. */
const announced = new Set<string>();

export function registerBitrixHandlers(island: Island) {
  void onEvent<BitrixUpdate>("bitrix", (update) => {
    State.bitrix = {
      configured: update.configured,
      tasks: update.tasks ?? State.bitrix.tasks,
      error: update.error,
      syncedAt: update.syncedAt ?? State.bitrix.syncedAt,
    };
    if (!update.configured) State.bitrix.tasks = [];
    scheduleReminder(island);
    State.notify();
  });
  // Rust only speaks up when the file changes: ask for the current list once.
  void Bridge.bitrixRefresh();
}

function scheduleReminder(island: Island) {
  if (reminder != null) window.clearTimeout(reminder);
  reminder = null;
  const at = nextAnnouncementAt(State.bitrix.tasks, Date.now(), announced);
  if (at == null) return;
  reminder = window.setTimeout(() => {
    reminder = null;
    const now = Date.now();
    const due = dueForAnnouncement(State.bitrix.tasks, now, announced);
    for (const t of due) announced.add(announceKey(t));
    const first = due[0];
    // Never in the middle of something else, and never while paused. Folded
    // into the thin bar, the bar says it until the deadline.
    if (first && !State.paused && island.quietBar) {
      Sound.play("pop");
      const more = due.length > 1 ? ` (+${due.length - 1})` : "";
      const left = deadlineMs(first) - now;
      island.notifyBar(`Due ${dueLabel(deadlineMs(first), now)} · ${first.title}${more}`, BITRIX_COLOR, Math.max(10_000, left));
    } else if (first && !State.paused && State.mode !== "expanded") {
      Sound.play("pop");
      island.setView("bitrix");
    }
    scheduleReminder(island);
  }, Math.min(MAX_WAIT_MS, Math.max(0, at - Date.now())));
}
