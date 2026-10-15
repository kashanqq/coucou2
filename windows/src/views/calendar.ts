// Calendar view — the week ahead from Google Calendar, grouped by day. Mochi
// sits on the left like in the chat; the list scrolls on the right.

import { h, svg, clear, dot } from "./dom";
import { ICONS } from "./icons";
import { Bridge, type CalendarEvent } from "../core/bridge";
import { State } from "../core/state";
import { CALENDAR_COLOR } from "../core/brand";
import type { ViewActions, ViewHost } from "./views";

const DAY_MS = 86_400_000;

/** All-day dates are calendar dates, not instants: read them as local midnight. */
export function eventStart(e: CalendarEvent): Date {
  return e.allDay ? localDate(e.start) : new Date(e.start);
}

export function eventEnd(e: CalendarEvent): Date {
  return e.allDay ? localDate(e.end) : new Date(e.end);
}

function localDate(ymd: string): Date {
  const [y, m, d] = ymd.split("-").map(Number);
  return new Date(y, (m || 1) - 1, d || 1);
}

function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

const timeFmt = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });
const dayFmt = new Intl.DateTimeFormat(undefined, { weekday: "long", day: "numeric", month: "short" });

function dayLabel(dayStart: number, today: number): string {
  if (dayStart === today) return `Today · ${dayFmt.format(dayStart)}`;
  if (dayStart === today + DAY_MS) return `Tomorrow · ${dayFmt.format(dayStart)}`;
  return dayFmt.format(dayStart);
}

function ago(ms: number): string {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return "synced just now";
  const m = Math.round(s / 60);
  return m < 60 ? `synced ${m} min ago` : `synced ${Math.round(m / 60)} h ago`;
}

/** Events from the start of today on, sorted, each placed on every day it covers. */
function groupByDay(events: CalendarEvent[], now: Date): Map<number, CalendarEvent[]> {
  const today = startOfDay(now);
  const days = new Map<number, CalendarEvent[]>();
  const sorted = [...events].sort((a, b) =>
    eventStart(a).getTime() - eventStart(b).getTime() || Number(b.allDay) - Number(a.allDay));
  for (const e of sorted) {
    const start = eventStart(e);
    const end = eventEnd(e);
    if (end.getTime() <= today) continue;
    // A multi-day event shows on each of its days (all-day end dates are exclusive).
    let day = Math.max(startOfDay(start), today);
    const last = end.getTime() - 1;
    for (let i = 0; i < 9 && day <= last; i++) {
      if (!days.has(day)) days.set(day, []);
      days.get(day)!.push(e);
      day = startOfDay(new Date(day + DAY_MS + 3_600_000)); // + 1 h survives DST
    }
  }
  return days;
}

function eventRow(e: CalendarEvent, now: number, actions: ViewActions): HTMLElement {
  const start = eventStart(e).getTime();
  const end = eventEnd(e).getTime();
  const live = !e.allDay && start <= now && now < end;
  const past = end <= now;
  const soon = !e.allDay && start > now && start - now <= 15 * 60_000;

  const time = e.allDay ? "All day" : timeFmt.formatRange(start, end);
  const info = h("div", { class: "cal-info" },
    h("div", { class: "cal-title", text: e.title }),
    e.location ? h("div", { class: "cal-loc", text: e.location }) : null,
  );
  const row = h("div", {
    class: `cal-row${live ? " live" : ""}${past ? " past" : ""}`,
    title: `${e.calendar}${e.htmlLink ? " — click to open in Google Calendar" : ""}`,
    onclick: () => { if (e.htmlLink) actions.openUrl(e.htmlLink); },
  },
    h("i", { class: "cal-bar", style: `background:${e.color}` }),
    h("div", { class: "cal-time", text: time }),
    info,
  );
  if (live) row.append(h("span", { class: "cal-badge", text: "now" }));
  else if (soon) row.append(h("span", { class: "cal-badge soon", text: `in ${Math.max(1, Math.round((start - now) / 60_000))} min` }));
  if (e.meetUrl && !past) {
    row.append(h("button", {
      class: "btn primary cal-join",
      text: "Join",
      onclick: (ev: Event) => {
        ev.stopPropagation();
        actions.openUrl(e.meetUrl!);
      },
    }));
  }
  return row;
}

export function buildCalendar(actions: ViewActions): ViewHost {
  const status = h("span", { class: "cal-status" });
  const refresh = h("button", {
    class: "icon-btn cal-icon",
    title: "Sync now",
    onclick: () => {
      actions.blip();
      refresh.classList.add("spin");
      void Bridge.calendarRefresh().finally(() => refresh.classList.remove("spin"));
    },
  }, svg(ICONS.refresh, 10, { stroke: 2.6 }));
  const open = h("button", {
    class: "icon-btn cal-icon",
    title: "Open Google Calendar",
    onclick: () => actions.openUrl("https://calendar.google.com"),
  }, svg(ICONS.arrowUpRight, 9));
  const head = h("div", { class: "cal-head" },
    dot(CALENDAR_COLOR, 7),
    h("span", { class: "cal-name", text: "Google Calendar" }),
    status,
    h("div", { class: "grow" }),
    refresh,
    open,
  );
  const list = h("div", { class: "cal-list" });
  const body = h("div", { class: "cal-body" }, head, list);
  const el = h("div", { class: "view" }, h("div", { class: "card cal-card" }, body));

  let key = "";
  let scrolledFor = "";

  return {
    el,
    sync() {
      const cal = State.calendar;
      const now = new Date();
      // Re-render when the data changes or the minute ticks over (for "now").
      const nextKey = [
        cal.connected, cal.error, cal.syncedAt, cal.events.length,
        cal.events.map((e) => `${e.id}${e.start}`).join(","), Math.floor(now.getTime() / 60_000),
      ].join("|");
      status.textContent = cal.connected && cal.syncedAt ? `· ${ago(cal.syncedAt)}` : "";
      refresh.style.display = cal.connected ? "" : "none";
      if (nextKey === key) return;
      key = nextKey;
      clear(list);

      if (!cal.connected) {
        list.append(h("div", { class: "cal-empty" },
          h("div", { class: "title", text: "Google Calendar isn't connected." }),
          h("div", { class: "sub", text: "Add your OAuth client in Settings, then click Connect." }),
          h("div", { class: "actions" },
            h("button", { class: "btn primary", text: "Open Settings", onclick: () => actions.openSettingsWindow() }),
          ),
        ));
        return;
      }
      if (cal.error) list.append(h("div", { class: "detail cal-error", text: cal.error }));

      const days = groupByDay(cal.events, now);
      if (days.size === 0) {
        list.append(h("div", { class: "cal-empty" },
          h("div", { class: "title", text: cal.syncedAt ? "Nothing on your calendar this week." : "Syncing…" }),
        ));
        return;
      }
      const today = startOfDay(now);
      const nowMs = now.getTime();
      let liveRow: HTMLElement | null = null;
      for (const [day, events] of days) {
        list.append(h("div", { class: "cal-day", text: dayLabel(day, today) }));
        for (const e of events) {
          const row = eventRow(e, nowMs, actions);
          if (!liveRow && !row.classList.contains("past")) liveRow = row;
          list.append(row);
        }
      }
      // Open on what's happening now, not on this morning's finished meetings —
      // but only on fresh data, so the minute tick doesn't yank a scrolled list.
      const dataKey = nextKey.slice(0, nextKey.lastIndexOf("|"));
      if (dataKey === scrolledFor) return;
      scrolledFor = dataKey;
      if (liveRow) requestAnimationFrame(() => {
        list.scrollTop = Math.max(0, liveRow!.offsetTop - list.offsetTop - 22);
      });
    },
  };
}
