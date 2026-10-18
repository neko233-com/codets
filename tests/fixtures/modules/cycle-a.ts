// One half of an import cycle: this module imports cycle-b.ts, which imports
// this file back. Loading must terminate and both sides must see a namespace.

import { fromB } from "./cycle-b.ts";

export const fromA = "A";

export function aValue(): string {
  return `${fromA}${fromB}`;
}

console.log("cycle ok", aValue());
