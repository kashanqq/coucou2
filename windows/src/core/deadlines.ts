// Bitrix24 deadlines — pure logic, no DOM and no imports, so `npm test` can run
// it straight under Node.

export interface DeadlineTask {
  id: string;
  title: string;
  url: string | null;
  /** RFC 3339 date-time, e.g. 2026-10-20T18:00:00+05:00. */
  deadline: string;
}

/** A task needs attention once its deadline is less than this away. */
export const ATTENTION_MS = 2 * 60 * 60_000;

const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;

export interface TaskGroups {
  /** Most recently missed first. */
  overdue: DeadlineTask[];
  /** Due within ATTENTION_MS, soonest first. */
  attention: DeadlineTask[];
  /** Soonest first. */
  upcoming: DeadlineTask[];
}

export function deadlineMs(t: DeadlineTask): number {
  return Date.parse(t.deadline);
}

export function groupTasks(tasks: DeadlineTask[], now: number): TaskGroups {
  const dated = tasks
    .map((t) => ({ t, at: deadlineMs(t) }))
    .filter(({ at }) => Number.isFinite(at))
    .sort((a, b) => a.at - b.at);
  const groups: TaskGroups = { overdue: [], attention: [], upcoming: [] };
  for (const { t, at } of dated) {
    if (at <= now) groups.overdue.unshift(t);
    else if (at - now < ATTENTION_MS) groups.attention.push(t);
    else groups.upcoming.push(t);
  }
  return groups;
}

export interface DayColumn {
  /** "overdue" before today, a calendar "day", or "later" past the last day shown. */
  kind: "overdue" | "day" | "later";
  /** Local midnight of the day, Unix ms; null for "overdue" and "later". */
  day: number | null;
  /** Soonest first (overdue: most recently missed first). */
  tasks: DeadlineTask[];
}

function localMidnight(ms: number, plusDays = 0): number {
  const d = new Date(ms);
  // Built from the calendar date, so a DST change can't shift the boundary.
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() + plusDays).getTime();
}

/**
 * The calendar strip: overdue first, then every day from today — empty ones
 * too — up to the last deadline (at least `minDays`, at most `maxDays`), then
 * one "later" column so nothing further out is ever dropped.
 */
export function dayColumns(tasks: DeadlineTask[], now: number, minDays = 7, maxDays = 31): DayColumn[] {
  const { overdue, attention, upcoming } = groupTasks(tasks, now);
  const today = localMidnight(now);
  const end = localMidnight(now, maxDays);
  const future = [...attention, ...upcoming];

  const inRange = future.filter((t) => deadlineMs(t) < end);
  const lastDay = inRange.reduce((last, t) => Math.max(last, localMidnight(deadlineMs(t))), today);
  const columns: DayColumn[] = [];
  if (overdue.length > 0) columns.push({ kind: "overdue", day: null, tasks: overdue });
  for (let i = 0; i < maxDays; i++) {
    const day = localMidnight(now, i);
    if (i >= minDays && day > lastDay) break;
    const next = localMidnight(now, i + 1);
    columns.push({ kind: "day", day, tasks: inRange.filter((t) => deadlineMs(t) >= day && deadlineMs(t) < next) });
  }
  const later = future.filter((t) => deadlineMs(t) >= end);
  if (later.length > 0) columns.push({ kind: "later", day: null, tasks: later });
  return columns;
}

/** Anything overdue or under ATTENTION_MS away — the tab's amber dot. */
export function needsAttention(tasks: DeadlineTask[], now: number): boolean {
  const g = groupTasks(tasks, now);
  return g.overdue.length + g.attention.length > 0;
}

/** "in 1 h 40 min", "25 min late", "in 2 d". */
export function dueLabel(deadline: number, now: number): string {
  const late = deadline <= now;
  const ms = Math.abs(deadline - now);
  let span: string;
  if (ms >= DAY) {
    span = `${Math.floor(ms / DAY)} d`;
  } else {
    const total = Math.max(1, late ? Math.floor(ms / MIN) : Math.ceil(ms / MIN));
    const h = Math.floor(total / 60);
    const m = total % 60;
    span = h === 0 ? `${m} min` : m === 0 ? `${h} h` : `${h} h ${m} min`;
  }
  return late ? `${span} late` : `in ${span}`;
}

/** A moved deadline gets a new key, so it is announced again. */
export function announceKey(t: DeadlineTask): string {
  return `${t.id}@${t.deadline}`;
}

/** Tasks that just entered the attention window and haven't been announced. */
export function dueForAnnouncement(
  tasks: DeadlineTask[], now: number, announced: Set<string>,
): DeadlineTask[] {
  return groupTasks(tasks, now).attention.filter((t) => !announced.has(announceKey(t)));
}

/** When the next unannounced task enters the window — `now` if one already has. */
export function nextAnnouncementAt(
  tasks: DeadlineTask[], now: number, announced: Set<string>,
): number | null {
  let next: number | null = null;
  for (const t of tasks) {
    const at = deadlineMs(t);
    if (!Number.isFinite(at) || at <= now || announced.has(announceKey(t))) continue;
    // The first millisecond it is strictly under ATTENTION_MS away.
    const enters = Math.max(now, at - ATTENTION_MS + 1);
    if (next == null || enters < next) next = enters;
  }
  return next;
}
