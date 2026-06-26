# `capsule/app/`: game data (optional)

LambdaDoom runs the **shareware `DOOM1.WAD`**, which `ldoom build` fetches automatically at
build time and bakes into the MicroVM image. For the default demo this directory can be
**empty**, you do not need to supply anything.

Drop a file here only to **override or extend** the game data:

- Put your own **IWAD** (e.g. a different `DOOM1.WAD`) here and the build uses it instead of
  fetching one. The capsule loads the first `*.wad` it finds under `/home/app/app`.
- Add **PWADs** or other assets alongside it to mod the demo.

Payloads are git-ignored (`*.wad`, `*.exe`, `*.dll`, …; see `.gitignore`), so no game data
lands in the repo. LambdaDoom ships only the shareware `DOOM1.WAD` plus GPLv2 Chocolate
Doom, fetched at build time, never the retail `DOOM.WAD` / `DOOM2.WAD`. See
[security.md](../../docs/security.md).
