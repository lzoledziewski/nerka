# Settings and where things are kept

⚙ **Settings** (`Ctrl+,`) holds what applies to every tab:

- **Times:** show local time (the GUI's default) or UTC; read an MF4 start
  time marked as local in this computer's zone.
- **Protocols:** which protocols are read in captures, and which datagrams
  are each one's (rules by port, range, address, VLAN; the most specific
  wins); load your [protocol definitions](protocol-definitions.md). A tab's
  **Files…** turns protocols on or off per capture.
- **Compressed MF4:** the unpack cache's size limit, its use, **Clear**.
- **Reload configuration:** read packs, presets, plots, checks, signals and
  preferences again (after editing a file by hand, or after another window
  changed them).

Each tab's menu has **Records…** (how its text files are split into rows)
and **Files…** (its files, their order, protocols, Merge by time).

## Where things are kept

Nothing is stored in the cloud. Your configuration folder is
`%APPDATA%\nerka\` on Windows, `~/.config/nerka/` on Linux:

| File or folder | Holds |
|---|---|
| `packs\` | [query packs](packs.md) (`*.nerka-pack`) |
| `presets\` | [presets](packs.md#presets) (`*.nerka-preset`) |
| `plots\` | kept [plots](signals.md#plots) |
| `signals` | saved signals and lifecycles, one spec per line |
| `checks` | saved [checks](checks.md), one spec per line |
| `protocols\` | protocol definitions loaded in Settings |
| `records` | how each text file was last split into records |
| `prefs` | preferences (modes, times, ports, …) |
| `history`, `search-history` | the Filter and Search boxes' histories |

Bookmarks are kept next to the log, as `<log>.nerka-marks`. Compressed MF4
data is unpacked into `%LOCALAPPDATA%\nerka\cache` (Linux:
`~/.cache/nerka`).

To move your setup to another machine, copy those files, or export a
[settings bundle](bundles.md):

```
nerka export -o mine.nerka-bundle
nerka import --dry-run mine.nerka-bundle
```

## Several windows

Each window is a program of its own, sharing one configuration. Saving
checks, signals or preferences in one window keeps what another window
saved meanwhile. When another window changed packs, presets, plots, checks
or signals, a bar says what, with **Reload** and **Not now**: nothing is
read by itself, so what you're looking at isn't changed under you.
