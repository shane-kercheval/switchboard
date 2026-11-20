import { createSubscriber } from "svelte/reactivity";

/// A wall clock shared by the agent cards' elapsed times. One `setInterval`
/// runs while at least one effect or template reads `now()`, and stops when
/// the last reader goes away, so idle screens hold no timer.
const TICK_MS = 1000;

let current = Date.now();

const subscribe = createSubscriber((update) => {
  current = Date.now();
  const id = setInterval(() => {
    current = Date.now();
    update();
  }, TICK_MS);
  return () => clearInterval(id);
});

/// The current time in ms. Read inside an effect or template, it re-runs that
/// reader once per second; read anywhere else, it is a plain `Date.now()`.
export function now(): number {
  if (!$effect.tracking()) return Date.now();
  subscribe();
  return current;
}
