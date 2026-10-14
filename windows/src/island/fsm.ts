// Island open/close FSM — port of IslandStateMachine.swift.
// No DOM, no Tauri: it only reports transitions.

export type FsmState = "hidden" | "petit" | "home" | "coucou";

export class IslandStateMachine {
  state: FsmState = "hidden";

  onTransition: ((from: FsmState, to: FsmState) => void) | null = null;

  /** home → petit delay, seconds. */
  homeToPetitDelay = 15;
  /** petit → hidden delay, seconds. */
  petitToHiddenDelay = 60;
  /** coucou → petit once the greeting animation ends (no hover). */
  greetAutoCollapseDelay = 0.6;
  /** coucou → petit while the mouse hovers the greeting. */
  greetHoverCollapseDelay = 10;
  /** An alert waiting for an answer stays open, even when the mouse leaves. */
  pinned = false;
  /** Hover mode: hovering the compact island opens it, clicking does not. */
  hoverExpand = false;
  /**
   * Thin-bar look: hovering the bar peeks, a click opens, and the island folds back
   * into the bar the moment the cursor leaves. Replaces hover-expand.
   */
  barMode = false;
  /** Something (the chat field) needs the island to stay open while the cursor is away. */
  holdOpen = false;
  /** Bar mode, cursor left the compact island → bar, seconds. */
  barLeaveDelay = 0.12;
  /** Bar mode, peek with nobody hovering (a notification) → bar, seconds. */
  barPeekDelay = 6;

  private petitHide: number | null = null;
  private homeCollapse: number | null = null;
  private greetCollapse: number | null = null;

  // ── Inputs ──────────────────────────────────────────────────────────────────

  launch() {
    this.cancelTimers();
    this.transition("coucou");
  }

  mouseEntered() {
    switch (this.state) {
      case "hidden":
        this.cancelTimers();
        this.transition("petit");
        break;
      case "petit":
        this.clear("petitHide");
        if (this.expandsOnHover) {
          this.cancelTimers();
          this.transition("home");
        }
        break;
      case "home":
        this.clear("homeCollapse");
        break;
      case "coucou":
        this.scheduleGreetCollapse(this.greetHoverCollapseDelay);
        break;
    }
  }

  /** `cursorLeft`: the cursor really left the island (not a timer-driven re-check). */
  mouseLeft(cursorLeft = false) {
    switch (this.state) {
      case "hidden":
        break;
      case "petit":
        this.schedulePetitHide(this.barMode && cursorLeft ? this.barLeaveDelay : undefined);
        break;
      case "home":
        if (cursorLeft && this.barMode && !this.pinned && !this.holdOpen) {
          this.cancelTimers();
          this.transition("hidden");
        } else if (cursorLeft && this.expandsOnHover && !this.pinned) {
          this.cancelTimers();
          this.transition("petit");
        } else {
          this.scheduleHomeCollapse();
        }
        break;
      case "coucou":
        this.clear("greetCollapse");
        this.transition("petit");
        break;
    }
  }

  click() {
    if (this.state !== "petit" || this.expandsOnHover) return;
    this.cancelTimers();
    this.transition("home");
  }

  /** Greeting animation finished (T.end). Doesn't override a running hover timer. */
  greetComplete() {
    if (this.state !== "coucou") return;
    if (this.greetCollapse == null) this.scheduleGreetCollapse(this.greetAutoCollapseDelay);
  }

  /** Non-alert work event: show compact from hidden. */
  reveal() {
    if (this.state !== "hidden") return;
    this.cancelTimers();
    this.transition("petit");
    this.schedulePetitHide();
  }

  /** Alert or explicit request: open straight to expanded. */
  forceHome() {
    this.cancelTimers();
    this.transition("home");
  }

  /// Explicit close (OK button, Escape, an alert being answered).
  forcePetit() {
    this.cancelTimers();
    this.transition("petit");
  }

  forceHidden() {
    this.cancelTimers();
    this.transition("hidden");
  }

  // ── Timers ──────────────────────────────────────────────────────────────────

  /** Hover mode opens straight from the compact island; the bar wants a click for that. */
  private get expandsOnHover(): boolean {
    return this.hoverExpand && !this.barMode;
  }

  private schedulePetitHide(delay?: number) {
    this.clear("petitHide");
    const seconds = delay ?? (this.barMode ? this.barPeekDelay : this.petitToHiddenDelay);
    this.petitHide = window.setTimeout(() => {
      this.petitHide = null;
      if (this.state === "petit") this.transition("hidden");
    }, seconds * 1000);
  }

  private scheduleHomeCollapse() {
    this.clear("homeCollapse");
    if (this.pinned) return;
    this.homeCollapse = window.setTimeout(() => {
      this.homeCollapse = null;
      if (this.state === "home") this.transition("petit");
    }, this.homeToPetitDelay * 1000);
  }

  private scheduleGreetCollapse(delay: number) {
    this.clear("greetCollapse");
    this.greetCollapse = window.setTimeout(() => {
      this.greetCollapse = null;
      if (this.state === "coucou") this.transition("petit");
    }, delay * 1000);
  }

  private clear(which: "petitHide" | "homeCollapse" | "greetCollapse") {
    const id = this[which];
    if (id != null) window.clearTimeout(id);
    this[which] = null;
  }

  cancelTimers() {
    this.clear("petitHide");
    this.clear("homeCollapse");
    this.clear("greetCollapse");
  }

  private transition(next: FsmState) {
    if (next === this.state) return;
    const from = this.state;
    this.state = next;
    this.onTransition?.(from, next);
  }
}
