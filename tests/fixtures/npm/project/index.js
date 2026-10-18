// Runs only after `npm install --registry <fixture registry>` has placed the
// packages in node_modules.
const { greet } = require('greeter');
const path = require('path');

console.log(greet('world'));
console.log(path.join('a', 'b', '..', 'c'));
console.log(process.argv.length, process.argv0.endsWith('.js'));
