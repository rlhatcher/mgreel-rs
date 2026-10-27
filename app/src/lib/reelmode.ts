// Where each reel shows up in the module's Reel mode: the colour of the Reel
// Activity Window, and where Organize selects it.

import type { Slot } from "./api";

/** The order on page 13 of the manual. Reels after white start again at blue. */
export const REEL_COLOURS = [
  { name: "blue", css: "#3d8be8" },
  { name: "mint", css: "#7cf5a6" },
  { name: "green", css: "#86d94a" },
  { name: "yellow", css: "#e6bf4c" },
  { name: "orange", css: "#e2954a" },
  { name: "red", css: "#d8303c" },
  { name: "pink", css: "#ee7cef" },
  { name: "white", css: "#ffffff" },
];

/** The knob's travel, 7 o'clock to 5 o'clock. */
const SWEEP = 300;

export type ReelSpot = {
  /** Place among the reels on the card, from 0. */
  position: number;
  count: number;
  colour: (typeof REEL_COLOURS)[number];
  /** Organize knob angle from 12 o'clock, clockwise, in degrees. */
  angle: number;
  clock: string;
};

/**
 * Reel mode for each reel, by slot index. Reels count in slot order with
 * empty slots skipped; nobody has checked yet what the module does with a
 * gap. Organize splits its travel into one zone per reel plus a last one at
 * full clockwise that makes a new reel. The zones are taken as equal, so the
 * angle is a guide, not a measurement.
 */
export function reelSpots(slots: Slot[]): Map<number, ReelSpot> {
  const present = slots.filter((s) => s.present);
  const zones = present.length + 1;
  return new Map(
    present.map((s, position) => {
      const angle = -SWEEP / 2 + (SWEEP * (position + 0.5)) / zones;
      const hour = Math.round(12 + angle / 30) % 12 || 12;
      return [
        s.index,
        {
          position,
          count: present.length,
          colour: REEL_COLOURS[position % REEL_COLOURS.length],
          angle,
          clock: `${hour} o'clock`,
        },
      ];
    }),
  );
}
