// The other half of the cycle. `fromA` is read before cycle-a.ts has finished
// evaluating, so it must resolve to undefined rather than throwing.

import { fromA } from "./cycle-a.ts";

export const fromB = "B";

export const seenEarly = typeof fromA;
