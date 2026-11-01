# Measurements: start-up and size

How long DbRover takes to open its window on a Mac, and how big its download and install are.
dbrover.org quotes these numbers, so this page records how they were taken. DBeaver Community,
DataGrip and DBX were measured the same way on the same machine, to check the method gives
sensible numbers for apps built in very different ways. Anyone with the same apps installed can
take them again with [`site/measure.sh`](../site/measure.sh).

## Results

| App | Window | Settled | Download | Installed |
| --- | --- | --- | --- | --- |
| DbRover 0.13.0 | 0.35 s | 0.41 s | 75 MiB, universal | 174 MiB |
| DBX 0.6.60 | 0.38 s | 0.42 s | 43 MiB, Apple silicon | 77 MiB |
| DataGrip 2026.2.6 | 1.8 s | 1.9 s | 980 MiB, Apple silicon | 2695 MiB |
| DBeaver Community 26.2.3 | 3.6 s | 3.7 s | 117 MiB, Apple silicon | 199 MiB |

**Window** and **Settled** are the medians of five warm starts, explained under
[Start-up](#start-up). DbRover's download holds an Apple-silicon and an Intel binary; the arm64
part of its binary is about 84 MiB.

Memory with a connection open was not measured: it needs someone to connect each app by hand,
and the site mentions start-up and size only as facts about DbRover, not as a comparison.

## The machine

| | |
| --- | --- |
| Computer | MacBook Pro, Apple M4 Pro (12 cores), 24 GB of memory |
| System | macOS 27.2 (26B123) |
| Measured | 2026-10-29 |
| Apps | DbRover 0.13.0 (Homebrew, `navasvarela/tap/dbrover`), DBeaver Community 26.2.3 (Homebrew, `dbeaver-community`), DataGrip 2026.2.6 (JetBrains' disk image), DBX 0.6.60 (its GitHub release) |

Other apps were open as on a normal working day (a browser, a terminal, chat). They compete
for the processor equally with each app measured; nothing measured ran at the same time as
another.

## Method

Each app runs on a configuration of its own, made for the measurement, so nobody's settings,
projects or plugins change the numbers, and the measurement doesn't touch them:

| App | How it gets a fresh configuration |
| --- | --- |
| DbRover | `HOME` points at an empty folder, which holds `.config/dbrover` and `.local/share/dbrover` |
| DBeaver | `-data` names an empty Eclipse workspace |
| DataGrip | `DATAGRIP_PROPERTIES` names a file that moves its config, system, plugins and log folders |
| DBX | Runs on the user's own home, because it opens the login keychain as it starts and macOS finds that keychain through `HOME`; on a Mac where DBX was just installed, its folder is empty |

`site/measure.sh setup APP` launches the app once on that configuration and waits while you take
it through its first-run screens by hand (DBeaver's welcome dialogs, DataGrip's agreement and
licence), then quit it. Every later launch then starts the way it does on any normal day.

### Start-up

`site/measure.sh startup APP` launches the app five times with `open -n` and quits it between
runs. [`site/window-probe.js`](../site/window-probe.js) reads two times from the moment just
before `open`:

- **Window:** the first on-screen window of the app at least 800 × 500 points. That size is
  above any splash screen or first-run dialog, and below each app's main window. The window
  list comes from CoreGraphics, which doesn't need the Screen Recording permission.
- **Settled:** the moment from which the app used under 5% of one core for two seconds, from
  `ps`'s CPU time every half second. The app counts with every process it started and, for an
  app whose window is a web view, the WebKit processes that appeared after it launched. A JVM
  app goes on loading plugins and indexing after its window is up, and this catches when it
  stops.

The table gives each time's median over the five runs. These are warm starts: the app's files
are in the system's cache from the run before. A cold start, after a reboot or `sudo purge`, is
slower for every app and wasn't measured.

### Size

`site/measure.sh sizes` asks each project's download server for the size of the disk image it
offers an Apple-silicon Mac, and measures the installed app with `du`. DbRover ships one
universal disk image, with both an Apple-silicon and an Intel binary in it. The others ship one
per architecture, so theirs hold one binary.

## Taking them again

```sh
site/measure.sh setup dbrover          # and dbeaver, datagrip, dbx
site/measure.sh startup dbrover        # five launches; repeat per app
site/measure.sh sizes
```

Results are appended to `$TMPDIR/dbrover-measure/results.tsv` (or `MEASURE_DIR`'s), one line
per number, with the app's version.

When a release changes start-up or size noticeably, measure again and update this page and the
numbers on dbrover.org together.
