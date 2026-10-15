// Bitrix24 view — tasks from tasks.json in three groups: overdue, needing
// attention (deadline under two hours away), and upcoming. Same frame as the
// calendar: Mochi on the left, the list scrolling on the right.

import { h, svg, clear, dot } from "./dom";
import { ICONS } from "./icons";
import { Bridge, type BitrixTask } from "../core/bridge";
import { State } from "../core/state";
import { BITRIX_COLOR } from "../island/bitrix";
import { deadlineMs, dueLabel, groupTasks } from "../core/deadlines";
import type { ViewActions, ViewHost } from "./views";

const DAY_MS = 86_400_000;

const timeFmt = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });
const dateFmt = new Intl.DateTimeFormat(undefined, { weekday: "short", day: "numeric", month: "short" });

function startOfDay(ms: number): number {
  const d = new Date(ms);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

/** "Today 18:00", "Tomorrow 9:00", "Tue 20 Oct 18:00". */
function deadlineText(at: number, now: number): string {
  const days = Math.round((startOfDay(at) - startOfDay(now)) / DAY_MS);
  const day = days === 0 ? "Today" : days === 1 ? "Tomorrow" : days === -1 ? "Yesterday" : dateFmt.format(at);
  return `${day} ${timeFmt.format(at)}`;
}

function updatedAgo(ms: number): string {
  const m = Math.round(Math.max(0, Date.now() - ms) / 60_000);
  if (m < 1) return "updated just now";
  if (m < 60) return `updated ${m} min ago`;
  const hours = Math.round(m / 60);
  return hours < 48 ? `updated ${hours} h ago` : `updated ${Math.round(hours / 24)} d ago`;
}

type Kind = "late" | "soon" | "later";

const BAR_COLOR: Record<Kind, string> = {
  late: "var(--red)",
  soon: "var(--amber)",
  later: BITRIX_COLOR,
};

function taskRow(t: BitrixTask, kind: Kind, now: number, actions: ViewActions): HTMLElement {
  const at = deadlineMs(t);
  const row = h("div", {
    class: `cal-row bx-row ${kind}`,
    title: t.url ? "Click to open in Bitrix24" : t.title,
    onclick: () => { if (t.url) actions.openUrl(t.url); },
  },
    h("i", { class: "cal-bar", style: `background:${BAR_COLOR[kind]}` }),
    h("div", { class: "cal-time", text: deadlineText(at, now) }),
    h("div", { class: "cal-info" }, h("div", { class: "cal-title", text: t.title })),
  );
  if (kind !== "later") {
    row.append(h("span", { class: `cal-badge ${kind}`, text: dueLabel(at, now) }));
  }
  return row;
}

/** The portal a task link points at, for the "open Bitrix24" button. */
function portalOf(tasks: BitrixTask[]): string | null {
  for (const t of tasks) {
    if (!t.url) continue;
    try {
      return new URL(t.url).origin;
    } catch {
      // not a URL — try the next one
    }
  }
  return null;
}

export function buildBitrix(actions: ViewActions): ViewHost {
  const status = h("span", { class: "cal-status" });
  const refresh = h("button", {
    class: "icon-btn cal-icon",
    title: "Re-read tasks.json",
    onclick: () => {
      actions.blip();
      refresh.classList.add("spin");
      void Bridge.bitrixRefresh().finally(() => window.setTimeout(() => refresh.classList.remove("spin"), 400));
    },
  }, svg(ICONS.refresh, 10, { stroke: 2.6 }));
  let portal: string | null = null;
  const open = h("button", {
    class: "icon-btn cal-icon",
    title: "Open Bitrix24",
    onclick: () => { if (portal) actions.openUrl(portal); },
  }, svg(ICONS.arrowUpRight, 9));
  const head = h("div", { class: "cal-head" },
    dot(BITRIX_COLOR, 7),
    h("span", { class: "cal-name", text: "Bitrix24" }),
    status,
    h("div", { class: "grow" }),
    refresh,
    open,
  );
  const list = h("div", { class: "cal-list" });
  const body = h("div", { class: "cal-body" }, head, list);
  const el = h("div", { class: "view" }, h("div", { class: "card cal-card" }, body));

  let key = "";

  return {
    el,
    sync() {
      const bx = State.bitrix;
      const now = Date.now();
      // Re-render when the data changes or the minute ticks over (for "in 40 min").
      const nextKey = [
        bx.configured, bx.error, bx.syncedAt, bx.tasks.length,
        bx.tasks.map((t) => `${t.id}${t.deadline}`).join(","), Math.floor(now / 60_000),
      ].join("|");
      status.textContent = bx.configured && bx.syncedAt ? `· ${updatedAgo(bx.syncedAt)}` : "";
      refresh.style.display = bx.configured ? "" : "none";
      portal = portalOf(bx.tasks);
      open.style.display = portal ? "" : "none";
      if (nextKey === key) return;
      key = nextKey;
      clear(list);

      if (!bx.configured) {
        list.append(h("div", { class: "cal-empty" },
          h("div", { class: "title", text: "No Bitrix24 task list yet." }),
          h("div", { class: "sub", text: "Point Coucou at your tasks.json in Settings." }),
          h("div", { class: "actions" },
            h("button", { class: "btn primary", text: "Open Settings", onclick: () => actions.openSettingsWindow() }),
          ),
        ));
        return;
      }
      if (bx.error) list.append(h("div", { class: "detail cal-error", text: bx.error }));

      const groups = groupTasks(bx.tasks, now);
      const sections: [string, Kind, BitrixTask[]][] = [
        ["Overdue", "late", groups.overdue],
        ["Needs attention", "soon", groups.attention],
        ["Upcoming", "later", groups.upcoming],
      ];
      if (sections.every(([, , tasks]) => tasks.length === 0)) {
        if (!bx.error) {
          list.append(h("div", { class: "cal-empty" },
            h("div", { class: "title", text: "No open tasks with a deadline." }),
          ));
        }
        return;
      }
      for (const [label, kind, tasks] of sections) {
        if (tasks.length === 0) continue;
        list.append(h("div", { class: `cal-day bx-group ${kind}`, text: `${label} · ${tasks.length}` }));
        for (const t of tasks) list.append(taskRow(t, kind, now, actions));
      }
    },
  };
}
