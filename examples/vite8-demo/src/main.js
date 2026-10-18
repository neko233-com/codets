// The browser entry point: what `index.html` loads and what `vite build` would
// bundle. It needs a DOM, so it is not what the verification script runs — that is
// `src/summary.ts`, which is pure and therefore runnable by `codets node`.
import { summarise, type Release } from './lib/stats.ts';

const releases: Release[] = [
  { name: 'vite', version: '8.x' },
  { name: 'codets', version: '0.3.0' },
];

document.querySelector('#app')!.textContent = summarise(releases);
