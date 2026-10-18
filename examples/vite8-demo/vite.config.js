// A Vite configuration, kept because the demo is a real Vite project rather than a
// folder of files that happen to list Vite as a dependency.
//
// `codets` cannot execute this: Vite's own command line needs `child_process` to
// spawn esbuild, and a native module for Rolldown, neither of which this toolchain
// implements. See the README next to this file for the whole list.
import { defineConfig } from 'vite';

export default defineConfig({
  root: '.',
  build: {
    outDir: 'dist',
  },
});
