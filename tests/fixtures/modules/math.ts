// A fixture module that exercises the parts of TypeScript the parser erases
// plus the two constructs that survive into the runtime.

export const PI: number = 3.14159;

export interface Shape {
  area(): number;
}

export enum Kind {
  Circle,
  Square = 10,
}

export function double(n: number): number {
  return n * 2;
}

export function describe(kind: Kind): string {
  return `kind=${kind} name=${Kind[kind]}`;
}

export default class Calculator {
  private readonly factor: number;

  constructor(private label: string, factor: number = 2) {
    this.factor = factor;
  }

  get name(): string {
    return this.label;
  }

  scale(values: number[]): number[] {
    return values.map((value) => value * this.factor);
  }
}
