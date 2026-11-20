import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { flushSync, tick } from "svelte";
import { now } from "./clock.svelte";

/// The shared clock holds one interval while anything reads it and none once
/// nothing does, so an idle window runs no timer.

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
  vi.setSystemTime(new Date("2026-05-16T00:00:00Z"));
});

afterEach(() => {
  vi.useRealTimers();
});

function reader(): { seen: () => number; stop: () => void } {
  let seen = 0;
  const stop = $effect.root(() => {
    $effect(() => {
      seen = now();
    });
  });
  flushSync();
  return { seen: () => seen, stop };
}

it("runs one interval for any number of readers and stops when the last one leaves", async () => {
  expect(vi.getTimerCount()).toBe(0);
  const first = reader();
  const second = reader();
  expect(vi.getTimerCount()).toBe(1);

  first.stop();
  await tick();
  expect(vi.getTimerCount()).toBe(1);

  second.stop();
  await tick();
  expect(vi.getTimerCount()).toBe(0);
});

it("ticks readers once a second", () => {
  const one = reader();
  const start = one.seen();
  vi.advanceTimersByTime(1000);
  flushSync();
  expect(one.seen()).toBe(start + 1000);
  one.stop();
});

it("reads the wall clock directly outside any reader, starting no timer", () => {
  expect(now()).toBe(Date.parse("2026-05-16T00:00:00Z"));
  expect(vi.getTimerCount()).toBe(0);
});
