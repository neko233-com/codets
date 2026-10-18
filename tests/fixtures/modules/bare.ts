// A bare specifier must be rejected with a message that explains why, rather
// than being silently resolved somewhere unexpected.

import { x } from "some-package";

console.log(x);
