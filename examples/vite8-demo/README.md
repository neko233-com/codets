# A Vite 8 project, installed by codets

This project exists to be installed by `codets npm install` and then run by
`codets node`. It is the end-to-end check that a real, large dependency tree from the
public registry lands on disk and works — nothing here is a fixture, and no part of
the tree is excluded to make it pass.

## What it verifies

`scripts/verify-demo.ps1` (Windows) or `scripts/verify-demo.sh`, run from the
repository root, does this and fails on the first thing that is wrong:

* cleans `node_modules` and the lockfile, so every run starts from the same place;
* reports which registry the install will use;
* runs `codets npm install`, and prints the summary and every warning it emitted;
* asserts that `node_modules/vite` is the 8.x line, that the lockfile lists the
  packages, and that `node_modules/.bin` has a `vite` shim;
* runs this project's own TypeScript with `codets node src/summary.ts` and checks the
  output — two modules joined by a relative import, an interface, a type annotation
  and a template literal, with no build step and no bundler.

It needs the network, so it is not part of `cargo test`. Run it before a release,
alongside `cargo test -- --ignored` with `CODETS_LIVE_REGISTRY=1`.

### Last recorded run

2026-10-19, Windows, `x86_64-pc-windows-msvc`, `registry.npmjs.org`, 8 jobs:

| | |
|---|---|
| Vite | 8.3.4 |
| Packages installed | 40 |
| Written to disk | 366.7 MiB |
| Time | 6.23s total, 2.11s in transfer |
| Installer warnings | 0 |

That is the whole point of a tree this size: it exercises the resolver, the parallel
fetch and the unpack path against packages nobody here wrote. This run is also what
found the 10 MiB response limit that made `npm install vite` impossible; see the
CHANGELOG.

## What it does not verify

**`vite dev`, `vite build` and `vite preview` cannot run here.** Vite's command line

* spawns esbuild as a child process, and `codets` does not implement
  `child_process`, and
* loads Rolldown and lightningcss as native `.node` modules, which `codets` refuses
  because a native addon binds V8's ABI.

Installing those packages proves the registry protocol, the resolver, the parallel
download and the extraction work on a real tree. It does not prove Vite runs, and the
verification scripts say so at the end of every run rather than leaving it to be
discovered later.

## Running it by hand

```console
$ cd examples/vite8-demo
$ codets npm install
$ codets node src/summary.ts
vite@8
codets@0.3
vite + codets: 2 releases described
```

`node_modules/` and `package-lock.json` here are generated and ignored by git; the
lockfile records whatever the registry served that day.
