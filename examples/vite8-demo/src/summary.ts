// The demo's own TypeScript, run directly: no bundler, no `ts-node`, no build step.
//
// This is the file `scripts/verify-demo.*` executes after `npm install`, so the demo
// verifies two separate things — that the Vite dependency tree installs, and that
// TypeScript in the same project runs.
import { describe, summarise, type Release } from './lib/stats.ts';

const releases: Release[] = [
  { name: 'vite', version: '8' },
  { name: 'codets', version: '0.3' },
];

for (const release of releases) {
  console.log(describe(release));
}
console.log(summarise(releases));
