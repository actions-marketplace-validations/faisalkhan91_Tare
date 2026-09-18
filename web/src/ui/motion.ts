// Small lifecycle helpers for CSS-driven overlay motion. CSS owns the visual timing; JavaScript only
// advances named states and waits for the actual computed transition before removing the surface.

function cssTimeMs(value: string): number {
  const token = value.trim();
  if (token.endsWith("ms")) return Number.parseFloat(token) || 0;
  if (token.endsWith("s")) return (Number.parseFloat(token) || 0) * 1_000;
  return 0;
}

function transitionTimeMs(element: HTMLElement, watchedProperty: string): number {
  const style = getComputedStyle(element);
  const properties = style.transitionProperty.split(",").map((value) => value.trim());
  const durations = style.transitionDuration.split(",").map(cssTimeMs);
  const delays = style.transitionDelay.split(",").map(cssTimeMs);
  if (properties.length === 0 || properties.every((property) => !property || property === "none")) {
    return 0;
  }
  let longest = 0;
  const count = Math.max(properties.length, durations.length, delays.length);
  for (let index = 0; index < count; index += 1) {
    const property = properties[index % properties.length];
    if (property !== "all" && property !== watchedProperty) continue;
    const duration = durations[index % Math.max(1, durations.length)] ?? 0;
    const delay = delays[index % Math.max(1, delays.length)] ?? 0;
    longest = Math.max(longest, duration + delay);
  }
  return Math.max(0, longest);
}

/**
 * Put an attached surface in its pre-enter state, force that state to be observed for one frame,
 * then advance it. The state guard prevents a fast close from being overwritten by a queued open.
 */
export function beginMotion(
  element: HTMLElement,
  attribute: string,
  initial = "opening",
  settled = "open"
): void {
  element.setAttribute(attribute, initial);
  queueMicrotask(() => {
    if (element.getAttribute(attribute) !== initial) return;
    const settle = (): void => {
      if (element.getAttribute(attribute) === initial) element.setAttribute(attribute, settled);
    };
    if (typeof requestAnimationFrame === "function" && element.isConnected) {
      // The read commits the initial transform before the next frame applies the settled state.
      element.getBoundingClientRect();
      requestAnimationFrame(settle);
    } else {
      settle();
    }
  });
}

/** Run `complete` once the watched transition finishes, with a bounded fallback for canceled events. */
export function afterTransition(
  element: HTMLElement,
  watchedProperty: string,
  complete: () => void
): void {
  let completed = false;
  let fallback: ReturnType<typeof setTimeout> | undefined;
  const finish = (): void => {
    if (completed) return;
    completed = true;
    element.removeEventListener("transitionend", onEnd);
    element.removeEventListener("transitioncancel", onEnd);
    if (fallback !== undefined) clearTimeout(fallback);
    complete();
  };
  const onEnd = (event: Event): void => {
    const transition = event as TransitionEvent;
    if (event.target === element && transition.propertyName === watchedProperty) finish();
  };
  const duration = transitionTimeMs(element, watchedProperty);
  if (duration <= 0) {
    queueMicrotask(finish);
    return;
  }
  element.addEventListener("transitionend", onEnd);
  element.addEventListener("transitioncancel", onEnd);
  fallback = setTimeout(finish, duration + 50);
}
