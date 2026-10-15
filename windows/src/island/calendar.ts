// Google Calendar events → island state, plus the meeting heads-up: five
// minutes before an event starts the island opens on the calendar. One timer
// for the next event only — nothing ticks in between.

import { Bridge, onEvent, type CalendarUpdate } from "../core/bridge";
import { Sound } from "../core/sound";
import { State } from "../core/state";
import { eventStart } from "../views/calendar";
import type { Island } from "./island";

const HEADS_UP_MS = 5 * 60_000;

let reminder: number | null = null;
/** `${id}@${start}` of every event already announced, so a resync can't repeat one. */
const announced = new Set<string>();

export function registerCalendarHandlers(island: Island) {
  let heard = false;
  const apply = (update: CalendarUpdate) => {
    State.calendar = {
      connected: update.connected,
      events: update.events ?? State.calendar.events,
      error: update.error,
      syncedAt: update.syncedAt ?? State.calendar.syncedAt,
    };
    if (!update.connected) State.calendar.events = [];
    scheduleReminder(island);
    State.notify();
  };
  void onEvent<CalendarUpdate>("calendar", (update) => {
    heard = true;
    apply(update);
  });
  // The week from the last sync (calendar.json), before the network answers —
  // unless a fresher update already arrived.
  void Bridge.calendarSnapshot().then((update) => {
    if (update && !heard) apply(update);
  });
}

function scheduleReminder(island: Island) {
  if (reminder != null) window.clearTimeout(reminder);
  reminder = null;
  const now = Date.now();
  const next = State.calendar.events
    .filter((e) => !e.allDay && !announced.has(`${e.id}@${e.start}`))
    .map((e) => ({ e, at: eventStart(e).getTime() }))
    .filter(({ at }) => at > now)
    .sort((a, b) => a.at - b.at)[0];
  if (!next) return;
  reminder = window.setTimeout(() => {
    reminder = null;
    announced.add(`${next.e.id}@${next.e.start}`);
    // Never in the middle of something else, and never while paused. Folded
    // into the thin bar, the bar says it until the meeting starts.
    if (!State.paused && island.quietBar) {
      Sound.play("pop");
      const left = Math.max(1, Math.round((next.at - Date.now()) / 60_000));
      island.notifyBar(`In ${left} min · ${next.e.title}`, next.e.color, Math.max(10_000, next.at - Date.now()));
    } else if (!State.paused && State.mode !== "expanded") {
      Sound.play("pop");
      island.setView("calendar");
    }
    scheduleReminder(island);
  }, Math.max(0, next.at - HEADS_UP_MS - now));
}
