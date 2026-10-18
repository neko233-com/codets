// A CommonJS package that depends on another one, so the fixture exercises
// require, the node_modules walk, and the installer's transitive resolution.
const shout = require('shout');

module.exports = {
  greet: (name) => shout(`hi ${name}`),
};
