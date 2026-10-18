// The entry module: mixes default, named and namespace imports.

import Calculator, { PI, Kind, double, describe } from "./math.ts";
import * as math from "./math.ts";

const calc = new Calculator("main", 3);

console.log(PI);
console.log(double(21));
console.log(calc.name);
console.log(calc.scale([1, 2]));
console.log(describe(Kind.Square));
console.log(Kind.Circle, Kind[10]);
console.log(math.double(2) + math.PI);
