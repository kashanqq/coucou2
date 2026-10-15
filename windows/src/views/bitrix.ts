// Bitrix24 view — a calendar strip: one column per day, left to right, starting
// with what is overdue. The strip scrolls sideways; a busy day scrolls on its
// own, so the island never grows taller. Same frame as the calendar: Mochi on
// the left, the strip on the right.

import { h, svg, clear, dot } from "./dom";
import { ICONS } from "./icons";
import { Bridge, type BitrixTask } from "../core/bridge";
import { State } from "../core/state";
import { BITRIX_COLOR } from "../island/bitrix";
import { ATTENTION_MS, dayColumns, deadlineMs, dueLabel, type DayColumn } from "../core/deadlines";
import type { ViewActions, ViewHost } from "./views";

const DAY_MS = 86_400_000;

const timeFmt = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });
const weekdayFmt = new Intl.DateTimeFormat(undefined, { weekday: "short" });
const dateFmt = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short" });

function updatedAgo(ms: number): string {
  const m = Math.round(Math.max(0, Date.now() - ms) / 60_000);
  if (m < 1) return "updated just now";
  if (m < 60) return `updated ${m} min ago`;
  const hours = Math.round(m / 60);
  return hours < 48 ? `updated ${hours} h ago` : `updated ${Math.round(hours / 24)} d ago`;
}

/** [main label, small label] for a column header. */
function columnLabel(col: DayColumn, today: number): [string, string] {
  if (col.kind === "overdue") return ["Overdue", String(col.tasks.length)];
  if (col.kind === "later") return ["Later", String(col.tasks.length)];
  const day = col.day!;
  const offset = Math.round((day - today) / DAY_MS);
  const main = offset === 0 ? "Today" : offset === 1 ? "Tomorrow" : weekdayFmt.format(day);
  return [main, dateFmt.format(day)];
}

type Kind = "late" | "soon" | "later";

function kindOf(at: number, now: number): Kind {
  if (at <= now) return "late";
  return at - now < ATTENTION_MS ? "soon" : "later";
}

const BAR_COLOR: Record<Kind, string> = {
  late: "var(--red)",
  soon: "var(--amber)",
  later: BITRIX_COLOR,
};

function taskCard(t: BitrixTask, col: DayColumn, now: number, actions: ViewActions): HTMLElement {
  const at = deadlineMs(t);
  const kind = kindOf(at, now);
  // Overdue and "later" columns aren't one day: their cards say which day.
  const when = col.kind === "day" ? timeFmt.format(at) : `${dateFmt.format(at)}, ${timeFmt.format(at)}`;
  const card = h("div", {
    class: `bx-card ${kind}`,
    title: t.url ? `${t.title}\nClick to open in Bitrix24` : t.title,
    onclick: () => { if (t.url) actions.openUrl(t.url); },
  },
    h("i", { class: "bx-card-bar", style: `background:${BAR_COLOR[kind]}` }),
    h("div", { class: "bx-card-body" },
      h("div", { class: "bx-card-time" },
        h("span", { text: when }),
        kind !== "later" ? h("span", { class: `cal-badge ${kind}`, text: dueLabel(at, now) }) : null,
      ),
      h("div", { class: "bx-card-title", text: t.title }),
    ),
  );
  return card;
}

function column(col: DayColumn, today: number, now: number, actions: ViewActions): HTMLElement {
  const [main, small] = columnLabel(col, today);
  const isToday = col.kind === "day" && col.day === today;
  const body = h("div", { class: "bx-col-body" });
  if (col.tasks.length === 0) body.append(h("div", { class: "bx-col-empty", text: "—" }));
  for (const t of col.tasks) body.append(taskCard(t, col, now, actions));
  return h("div", { class: `bx-col ${col.kind}${isToday ? " today" : ""}`, "data-key": `${col.kind}${col.day ?? ""}` },
    h("div", { class: "bx-col-head" }, h("span", { class: "bx-col-name", text: main }), h("span", { text: small })),
    body,
  );
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
  const strip = h("div", { class: "bx-strip" });
  // A mouse wheel has no sideways axis: over the strip it scrolls the days,
  // unless the pointer is in a day that can still scroll down (or up) itself.
  strip.addEventListener("wheel", (e) => {
    if (e.deltaY === 0 || Math.abs(e.deltaX) > Math.abs(e.deltaY)) return;
    const body = (e.target as Element).closest(".bx-col-body") as HTMLElement | null;
    if (body) {
      // Scaled displays leave fractional scroll positions: a pixel or two is "the end".
      const room = e.deltaY > 0
        ? body.scrollHeight - body.clientHeight - body.scrollTop > 2
        : body.scrollTop > 2;
      if (room) return;
    }
    strip.scrollLeft += e.deltaY;
    e.preventDefault();
  }, { passive: false });
  const body = h("div", { class: "cal-body" }, head, strip);
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
      // Keep the reader where they were across the minute tick and re-syncs.
      const keep = { left: strip.scrollLeft, tops: new Map<string, number>() };
      for (const c of strip.querySelectorAll<HTMLElement>(".bx-col")) {
        const scroller = c.querySelector<HTMLElement>(".bx-col-body");
        if (scroller && c.dataset.key) keep.tops.set(c.dataset.key, scroller.scrollTop);
      }
      key = nextKey;
      clear(strip);

      if (!bx.configured) {
        strip.append(h("div", { class: "cal-empty" },
          h("div", { class: "title", text: "No Bitrix24 task list yet." }),
          h("div", { class: "sub", text: "Enter your portal in Settings, or point Coucou at a tasks.json." }),
          h("div", { class: "actions" },
            h("button", { class: "btn primary", text: "Open Settings", onclick: () => actions.openSettingsWindow() }),
          ),
        ));
        return;
      }
      if (bx.error) strip.append(h("div", { class: "detail cal-error bx-error", text: bx.error }));

      const today = new Date(new Date(now).setHours(0, 0, 0, 0)).getTime();
      for (const col of dayColumns(bx.tasks, now)) {
        const el = column(col, today, now, actions);
        strip.append(el);
        const top = keep.tops.get(el.dataset.key!);
        if (top) el.querySelector<HTMLElement>(".bx-col-body")!.scrollTop = top;
      }
      strip.scrollLeft = keep.left;
    },
  };
}
