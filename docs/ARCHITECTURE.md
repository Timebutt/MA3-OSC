# MA3-OSC

A single standalone program that connects Behringer **X-Touch**, **X-Touch Extender**
(both in Mackie Control mode) and **X-Touch Compact** surfaces to **grandMA3 onPC** over
OSC. It replaces open-stage-control plus the `pam-OSC-Module.js` custom script. Runs on
macOS and Windows.

It is a port of [pam-osc](https://github.com/xxpasixx/pam-osc) (GPL-3.0). It reads the
same `mappings/*.json` files and also has a more readable v2 mapping format.

## Quick start

```sh
cargo build --release                       # binary: target/release/ma3-osc(.exe)
./target/release/ma3-osc --list-ports    # see what your surfaces are called
./target/release/ma3-osc --check         # validate config + mappings
./target/release/ma3-osc                 # run (menu-bar / tray icon)
./target/release/ma3-osc --install-autostart   # start it at every login
```

### macOS app

```sh
macos/make-app.sh --install    # builds MA3-OSC.app and copies it to /Applications
```

Then start it from Spotlight ("MA3-OSC"). It runs in the menu bar only, with no Dock icon.

- On its first start the app copies its starter config, `mappings/` and `plugin/ma3-osc.xml`
  to `~/Library/Application Support/MA3-OSC/`. Edit them there (*Open config folder* in
  the menu). Logs go to `logs/` in that folder. Rebuilding or reinstalling the app never
  overwrites these files.
- The starter config is the repo's `ma3-osc.json` at build time.
- The app is signed ad-hoc, which is enough on the Mac that built it. For other Macs, build
  with `SIGN_IDENTITY="Developer ID Application: …"` and notarize it. `UNIVERSAL=1` builds
  for both Apple silicon and Intel.
- If MA runs on another computer, macOS asks once for permission to access the local network.

### grandMA3 side

1. In MA: **Menu → In & Out → OSC**. Add or edit **line 2** so it sends to the computer running
   MA3-OSC (port 9004) and receives on port 9003. Enable "Receive", "Send" and "Receive Command".
   Leave **FaderRange** at 100 (MA's default), or set `"faderRange"` in `ma3-osc.json` to
   the same value. If they differ, the faders jump: MA reads the wrong scale and the motor faders
   follow its feedback.
2. Import `plugin/ma3-osc.xml` as a plugin. It contains two plugins:
   - **MA3-OSC Start Stop**: run it once to start sending, run it again to stop.
   - **MA3-OSC Settings**: the same settings page as pam-OSC (colors, names, timecode, fixed page).
3. Stop open-stage-control and the old pam-OSC plugin first. Both use UDP port 9004.

If the command line shows `Failed: SendOSC 2 ...`, MA could not send on OSC line 2: the
line doesn't exist, "Send" is off, or "Enable Output" (top of the OSC menu) is off. The
plugin prints what to check once in the System Monitor. To use a different line, run
`SetGlobalVariable "ma3OscEntry" 3` and restart the plugin.

The plugin is pam-OSC.lua with these fixes:

- It sends a heartbeat every second, so MA3-OSC knows when MA is gone.
- A Lua error is printed and the plugin keeps running.
- Names containing `"`, `,` or `;` no longer break messages.
- The undefined `faderEnd` bug is fixed.
- Executors set to **Fix** follow you to other pages. On page 2, a fader for executor 201
  that is fixed on page 1 shows and controls page 1's 201. A change to Fix takes up to a second
  to show up.
- It uses `IsRunningPlayback` on newer MA3 versions, so there is no deprecation warning.
- Running it while it is already running stops it. A show file saved while it was running
  does not block the next start.
- Executor buttons finish a command waiting in the command line, like executor keys on a
  console. See [Executor buttons and the command line](#executor-buttons-and-the-command-line).

### Executor buttons and the command line

On a console, `Assign` → tap Sequence 5 → press an executor key assigns the sequence to that
executor. An OSC key message (`/Page1/Key201`) only fires the executor, so MA3-OSC handles it:

1. The plugin reports MA's command line (`/cmdline`) whenever it changes.
2. If an executor button is pressed while the command line holds text, MA3-OSC sends
   `SetGlobalVariable "ma3OscTarget" "Page 1.201"` instead of the key press (and drops the
   button's release).
3. The plugin adds the executor to the waiting command, runs it and clears the command line.
   For `Assign`, `Copy`, `Move` and `Clone` with a source it adds `At`
   (`Assign Sequence 5 At Page 1.201`). Anything else gets the executor directly
   (`Store Page 1.201`, `Edit Page 1.201`, `Store Cue 5 Page 1.201`). The finished command
   is printed in the System Monitor.

With an empty command line, buttons work as before. To make a button always fire its
executor, add `"ignoreCmdline": true` to it in the mapping. This needs the MA3-OSC plugin:
with the old pam-OSC plugin the command line is never reported and nothing changes.

The old pam-OSC plugin also works with MA3-OSC, but without the link check.

To watch more executors, edit the `watch(...)` lines at the top of `plugin/ma3-osc.lua`.
Then run `python3 plugin/make_xml.py` and re-import the XML.

## Running

MA3-OSC shows an icon in the macOS menu bar or the Windows notification area:

| Icon | Meaning |
|---|---|
| green | grandMA3 link OK and at least one configured surface connected |
| orange | waiting for MA, no surface is plugged in, or MA runs the old plugin (no link check) |
| red | MA link lost, or a problem prevents starting (shown in the menu) |

The menu shows the status of MA and of each surface. It also has *Message monitor…*,
*Open log folder*, *Open config folder*, *Start at login* and *Quit*.

### Message monitor

*Message monitor…* (or starting with `--monitor`) opens a window that lists the traffic live.

- Each line shows the time, the direction, the device and the meaning of the message, for
  example `Note ch 1 note 104 vel 127`, `Fader ch 1 16383 (100%)`,
  `LCD X-Touch strip 2 bottom "Cue 3"` or `/Page1/Fader201 127`, plus the raw MIDI bytes.
- **MIDI in** and **OSC out** are shown by default. Tick **OSC in** (from MA) and **MIDI out**
  (to the surfaces: LEDs, motor faders, scribble strips) to see those as well.
- The filter box matches any text, for example `Fader201` or a device name.
- *Pause* holds the view while messages keep being collected. *Save…* writes the lines you
  can see to a text file in the log folder.
- Messages are only captured while the window is open. Closing it stops the capture, so it
  costs nothing during a show. The window keeps the last 5000 messages.
- On Windows it needs the Microsoft Edge WebView2 runtime, which is built into Windows 10 and 11.
- Without the tray (`--no-tray`), use `RUST_LOG=debug` to log every message instead.

- If something blocks the start, the problem is shown in the menu instead of the program
  exiting. Examples are a config typo or port 9004 already in use. For a port conflict it
  keeps retrying every 5 s, so closing open-stage-control is enough.
- `--no-tray` runs without an icon, for a terminal or a service.
- **Start at login:**
  - macOS: a LaunchAgent (`~/Library/LaunchAgents/org.ma3-osc.agent.plist`). It also
    restarts MA3-OSC if it ever crashes, but not after *Quit*.
  - Windows: the registry key `HKCU\…\Run`. No admin rights are needed.
  - Turn it on from the menu or with `--install-autostart` / `--uninstall-autostart`.
    The current config file is used.

## Configuration

The config file is `ma3-osc.json`. MA3-OSC looks for it in the current directory first,
then next to the executable, then (macOS) in `~/Library/Application Support/MA3-OSC/`.
You can also pass `--config <file>`.

```json
{
    "ma": { "host": "127.0.0.1", "port": 9003, "listenPort": 9004, "prefix": "", "faderRange": 100 },
    "devices": [
        { "name": "xtouch",   "match": "X-Touch",         "mapping": "mappings/xTouch1.json" },
        { "name": "extender", "match": "X-Touch-Ext",     "mapping": "mappings/xTouchExtender1.json" },
        { "name": "compact",  "match": "X-Touch Compact", "mapping": "mappings/xTouchCompact1.json" }
    ],
    "surfaceRefreshSecs": 10,
    "faderEchoGuardMs": 300,
    "heartbeatTimeoutMs": 3000
}
```

- **faderRange** must equal "FaderRange" on MA's OSC line (default 100). Fader values sent to
  MA are scaled to 0..faderRange. The old pam-OSC always sent 0..127 and needed FaderRange 127 in MA.
- **match** is compared to the MIDI port name, ignoring case.
  - An exact match wins over a substring match, and a port is never used twice. So
    "X-Touch" will not grab the Compact or the Extender.
  - Check the real port names with `--list-ports`.
  - If the input and output ports have different names, use `"input"` and `"output"`.
- A device listed in the config but not plugged in is fine. It connects as soon as it
  appears, and again after an unplug or power cycle, with the complete state
  (faders, LEDs, scribble strips) restored.
- **surfaceRefreshSecs** re-sends the full surface state periodically. This recovers a
  daisy-chained surface that rebooted without any USB change. Set it to 0 to disable.
- Unknown keys or typos in the config or a mapping file are reported with the exact location
  of the problem. It never silently falls back to a default.
- Logs go to `logs/` next to the config: daily files, 14 days kept.
  `RUST_LOG=debug` logs every message.

## Agent access (MCP)

MA3-OSC can run an [MCP](https://modelcontextprotocol.io) server, so an AI agent such as
Claude Code can read the desk. It is off unless the config has an `mcp` key:

```json
    "mcp": { "port": 9005 }
```

Then connect the agent, for example:

```sh
claude mcp add --transport http ma3 http://127.0.0.1:9005/mcp
```

- The server listens on 127.0.0.1 only. It refuses requests whose Host or Origin header is
  not local, so a web page cannot reach it through DNS rebinding.
- All tools are read-only:
  - `ma_status`: the bridge (MA link, connected surfaces) and grandMA3 (version, current
    page, command line, selected fixture count, desk lock, master states). The bridge part
    is answered even when MA is unreachable.
  - `ma_list_executors`: the executors on a page (default: the current page), with the
    assigned object, its current cue, whether it runs, and the fader and key functions.
  - `ma_list_pool`: number and name of the objects in a pool (`sequences`, `groups`,
    `macros`, `pages`, `timecodes`, `plugins`, `worlds`, `filters`, `matricks`, `layouts`,
    or `presets` with `presetPool`), at most 500 per call via `offset` and `limit`.
  - `ma_list_fixtures`: every fixture in the patch, depth first: grouping fixtures, the
    fixtures below them and their sub-fixtures, each with `depth` and `parent`. It gives FID
    and CID (always as text, e.g. "101.1"), name, fixture type, DMX mode, DMX address,
    layer, class and stage. MA reports a missing value as "None"; the tool leaves it out. It returns at most 300
    fixtures per call (default 100), paged with `offset` and `limit`. It reads
    Patch → Stages → each stage's Fixtures. Group contents are not listed: MA's Lua API
    only shows them by selecting the group, which would change the programmer.
- The tools need the current MA3-OSC plugin running in grandMA3. With an older plugin
  (no heartbeat) or no link, they fail at once with an explanation instead of waiting.
- Show-file text (names, labels) is passed to the agent as data. The server's instructions
  tell the agent not to follow requests found in it.

### How a request reaches MA

grandMA3 has no OSC request/reply, so the channel is built from what MA offers
(`src/rpc.rs` and the RPC part of `plugin/ma3-osc.lua`):

1. MA3-OSC sends OSC `/cmd SetGlobalVariable "ma3OscRpc" "<id>:<base64>"`. The plaintext
   is the request name, then one `key=value` line per argument.
2. The plugin polls that variable at 10 Hz, clears it and runs the request.
3. It answers with `/ma3osc/rpc ,siis <id> <index> <count> <chunk>`. The chunks join into
   base64 of `{"ok":true,"result":...}` or `{"ok":false,"error":"..."}`.

- One request is in flight at a time, because there is one request variable.
- A request must fit in one packet (at most 1,800 base64 characters). A reply is split
  into chunks of 1,700 characters (about 1,740-byte packets), at most 64 of them.
- A missing chunk shows up as a timeout: 2 s without a new chunk. The timeout counts from
  the last chunk, because MA sends about one chunk per 30 ms and a 50-chunk reply takes
  longer than 2 s. Requests are read-only, so it is sent once
  more under a new id. A late reply to the old id is ignored.
- Before the first request, and again after any failure, a `hello` handshake checks
  that the plugin speaks the same protocol (`PROTOCOL` in `rpc.rs` equals
  `RPC_PROTOCOL` in the plugin; raise both when the format changes) and reads `Version()`.
- Requests appear in the message monitor as OSC out, replies as OSC in.

## grandMA3 2.5 behaviour (measured)

The numbers below were measured on grandMA3 onPC 2.5.1.1 (macOS arm64) with a throwaway probe
plugin. They are behaviour the documentation does not state or gets wrong. Check them
again before relying on them with another MA version.

- **`Cmd()` returns `"OK"`** (capitals) on success. Earlier code compared with `"Ok"`, so
  every success counted as a failure. The plugin now compares without regard to case.
- **Errors are MA's own text, not the documented "Syntax Error"/"Illegal Command".**
  An unknown keyword returns `"Object not found"`, because MA reads
  `Ma3ProbeNotAKeyword 1` as `Channel "Ma3ProbeNotAKeyword" 1`. A missing object returns
  `"Illegal object"`. Treat anything other than "OK" as a failure and show the text.
- **OSC packets are limited to 2,048 bytes in both directions, and failures are silent.**
  - Out of MA (SendOSC): a packet of about 2,044 bytes arrived; one of about 2,084 bytes
    did not. MA logs `OSC output packet size exceeded.` in the System Monitor, drops the
    message, and `Cmd` still returns "OK".
  - Into MA (`/cmd`): 2,048 bytes of UDP arrived intact. A larger packet is cut to about
    2,048 bytes (the variable got 2,003 characters), and the command still runs and
    returns "OK" with the shortened text.
  - So: keep every packet well below 2 KB, and make truncation detectable (chunk counts,
    an end marker, a decode check).
- macOS refuses to send a UDP datagram over about 9 KB (`Message too long`).
- **SendOSC carries base64 unchanged** (`+`, `/`, `=`), and multi-argument type strings
  such as `siis` work.
- **Lua functions present in 2.5.1.1:** `CreateUndo`, `CloseUndo`, `CmdIndirect`,
  `CmdIndirectWait`, `SetProgPhaser` (not documented), `GetUIChannelIndex`,
  `GetAttributeIndex`, `SelectionFirst`, `SelectionCount`, `CmdObj`, `GetPath`, `HostOS`.
  **Missing:** `CmdlineIndex`. `Version()` returns `"2.5.1.1"`.
- **MA logs every `/cmd` it receives and every SendOSC** in the System Monitor ("OK: …"
  and `OSC::Data::CommandSendOSC 2`). Busy OSC traffic fills that log. That is one more
  reason the plugin sends only changes.
- **Latency:** the plugin picks up a request within its 100 ms poll interval. A `Cmd` call
  takes about 20–35 ms because it waits for MA's main task, so a reply of many chunks takes
  correspondingly longer.
- Any OSC from MA counts as a sign of life for the link watchdog. Traffic from another
  plugin (here, the probe) while the MA3-OSC plugin is stopped makes the bridge see the link
  as "restored" and ask for a reload (`forceReload`) each time.

## Mapping files

### v2 format (recommended for new mappings)

```json
{
    "format": 2,
    "label": "my X-Touch",
    "surface": "x-touch",
    "timecodeDisplay": true,
    "faders":   [ {"pitch": 1, "exec": 201, "label": "Front"}, {"cc": 70, "exec": 290} ],
    "encoders": [ {"cc": 16, "steps": {"up": [1, 8], "down": [65, 72]}, "exec": 401,
                   "ring": {"cc": 48, "from": 32, "to": 43}},
                  {"cc": 60, "steps": {"up": [1, 1], "down": [65, 65]}, "attribute": "current", "amount": 1.0} ],
    "buttons":  [ {"note": 16, "exec": 201},
                  {"note": 46, "cmd": "Previous Page", "minVelocity": 101},
                  {"note": 48, "action": "encoderFine"},
                  {"note": 50, "action": "selectAttribute", "attribute": "Dimmer"} ],
    "strips":   [ {"strip": 0, "exec": 201} ]
}
```

- **surface**: `x-touch`, `x-touch-extender` or `midi` (Compact, APC mini, Launchpad, …).
- **buttons** take one of `exec`, `cmd` or `quickey`.
  - **action** can be `encoderFine`, `encoderRough`, `selectAttribute`, `timecodeSelect` or
    `timecodePlayPause`.
  - `minVelocity: 101` reacts to the press only.
  - `fixedLed` sets an LED value that ignores MA.
  - `feedback: {"on": 127, "off": 0}` sets the LED values for this button.
  - `ignoreCmdline: true` always fires the executor, even when the command line waits for
    a destination.
- **buttonFeedback** (top level) is the default on/off LED value.
- **midiChannel** is the default channel for LED feedback on `midi` surfaces.
- In X-Touch modes, fader-touch notes 104–112 are added automatically. Override them with
  `"faderTouch": [{"note": 104, "exec": 201}]`.
- Every entry can carry a `"label"` for your own notes.

Convert an existing mapping:

```sh
ma3-osc --convert mappings/xTouch1.json mappings/xTouch1-v2.json
ma3-osc --convert mappings/xTouch1.json        # print to the terminal
```

The conversion is checked: the new file must load to exactly the same mapping, or nothing
is written.

### Legacy pam-OSC format

The old files keep working, with a few changes:

- `buttonFeedbackMapper` functions are no longer run as JavaScript. The usual
  `if (value == 'On') return X; if (value == 'Off') return Y;` form is recognised.
  For anything else, use `"feedback": { "on": 127, "off": 0 }` on the note.
- `"mode": "mc-extender"` selects the X-Touch Extender.
- `"faderTouch": { "104": 201 }` maps a touch-sense note to its fader executor.
- `"display": { "0": 201, ... }` maps a scribble strip to an executor.

### X-Touch Extender

`mappings/xTouchExtender1.json` is an example for an Extender next to an X-Touch that uses
`xTouch1.json`:

| Extender control | Executors |
|---|---|
| faders and strips | 210–217 |
| encoders and encoder buttons | 409–416 |
| REC buttons | 309–316 |
| SOLO and SELECT buttons | 210–217 |
| MUTE buttons | 110–117 |

These are all inside the ranges the plugin watches.

Each scribble strip shows the sequence name (top row), the cue name (bottom row) and the
appearance color.

## What's improved over the JS script

- **Motor faders don't fight your hand.** While a fader is touched, and for a short time
  after it moves, feedback from MA is held back, then applied.
- **No fader chatter.** A fader resting between two values, or a motor settling next to its
  target, makes the surface send a stream of alternating values. pam-OSC forwarded every
  one of them, which flooded MA and could flip X-fade executors back and forth. MA3-OSC
  ignores a reversal by a single step (out of 127). Moves in one direction keep full
  resolution, and 0 and 127 are always reachable. With an absolute encoder, turning back
  costs one click.
- **Plug and play.** It reconnects automatically and always resends the full state.
- **Clean scribble strips.** Accented characters become plain letters and other symbols
  become `?`. Previously they corrupted the SysEx message.
- Encoder values outside the configured ranges are ignored. They used to reset the
  executor to 0.
- Play/pause on an empty timecode slot no longer crashes.
- **MA link watchdog.** With the MA3-OSC plugin, the strips show red `MA LINK / LOST` and
  the tray icon turns red when MA goes quiet. When MA comes back, the bridge asks it for a
  full reload.
- A bug while handling one message is logged and skipped. It never stops the program.
- **SendOSC failures are reported correctly.** grandMA3 2.5 answers "OK", not "Ok". The old
  comparison treated every send and every Assign/Edit as failed.
- **Agent access.** An optional, read-only MCP server (see [Agent access](#agent-access-mcp)).

## Development

```sh
cargo test     # protocol, mapping, engine and MA-plugin tests (no hardware or MA needed)
cargo check --target x86_64-pc-windows-msvc   # Windows type-check from a Mac
```

| File | Purpose |
|---|---|
| `src/engine.rs` | all bridge behaviour (pure, tested) |
| `src/surface.rs` | desired-state model per surface: diff-only output, resync, fader holds |
| `src/mapping.rs` | mapping model and legacy loader |
| `src/mapping_v2.rs` | v2 format and converter |
| `src/xtouch.rs` | scribble-strip text and colors, 7-segment display |
| `src/midi_io.rs` | port matching and hot-plug |
| `src/osc_io.rs` | UDP OSC |
| `src/config.rs` | app config |
| `src/mcp.rs` | MCP server for agents (HTTP on 127.0.0.1, read-only tools) |
| `src/rpc.rs` | request/reply channel to the MA plugin: chunking, timeout, retry, handshake |
| `src/tray.rs`, `src/status.rs` | tray icon and status |
| `src/monitor.rs`, `src/monitor_view.rs`, `src/monitor_view.html` | message monitor capture and window |
| `src/autostart.rs` | start at login |
| `src/app_bundle.rs` | macOS app: config in Application Support, first-run setup |
| `macos/` | `make-app.sh`, `Info.plist` template and icon generator for `MA3-OSC.app` |
| `plugin/` | grandMA3 plugin (`make_xml.py` builds the XML) |
| `tests/ma_plugin.rs` | runs the Lua plugin against a mocked MA API |
