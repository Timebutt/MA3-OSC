# MA3-OSC

MA3-OSC lets you run **grandMA3 onPC** from Behringer **X-Touch**, **X-Touch Extender**
and **X-Touch Compact** surfaces. It also works with other MIDI controllers, such as the
Akai APC mini, Launchpad and MPX16. It is one standalone program for macOS and Windows that
translates between the surfaces' MIDI and MA's OSC.

It replaces the open-stage-control + `pam-OSC-Module.js` setup from
[pam-osc](https://github.com/xxpasixx/pam-osc) (GPL-3.0), which it is a port of. It reads
the same mapping files, so existing pam-OSC setups keep working.

## What it does

- **Faders, encoders, buttons → MA.** Surface input becomes executor, command and attribute
  OSC messages for MA.
- **MA → surface.** Motor faders, button LEDs, encoder rings, scribble strips (names and
  colors) and the timecode display follow the show.
- **Runs in the tray.** It runs as a menu-bar or tray icon with a live status for MA and each
  surface, a message monitor, and an option to start at login.
- **Plug and play.** Surfaces can be plugged in, unplugged or power-cycled at any time, and
  their full state is restored. A heartbeat from the MA plugin shows when the link to MA is lost.
- **Better behaved than the JS script.** Motor faders don't fight your hand, faders don't
  chatter, and config errors are reported with their exact location.
- **Agent access (optional).** A read-only [MCP](https://modelcontextprotocol.io) server lets
  an AI agent such as Claude Code read the desk: status, executors, pools and the patch.

## Repository layout

| Path | Contents |
|---|---|
| `src/` | the Rust program (bridge engine, MIDI/OSC I/O, tray, message monitor) |
| `plugin/` | grandMA3 Lua plugin (`ma3-osc.xml` to import into MA) |
| `mappings/` | mapping files for supported controllers |
| `ma3-osc.json` | starter config: MA address and which surface uses which mapping |
| `macos/` | builds the `MA3-OSC.app` menu-bar app |
| `tests/`, `examples/` | tests (no hardware or MA needed) and MIDI debug tools |
| `docs/` | detailed documentation |

## Quick start

```sh
cargo build --release
./target/release/ma3-osc --list-ports   # find your surfaces' MIDI port names
./target/release/ma3-osc --check        # validate config and mappings
./target/release/ma3-osc                # run
```

On a Mac, `macos/make-app.sh --install` builds and installs the menu-bar app instead.

In MA, set up OSC line 2 to send to port 9004 and receive on 9003, then import
`plugin/ma3-osc.xml`.

## Agent access (MCP)

MA3-OSC can run an MCP server so an AI agent can look at the show. It is off by default.
To turn it on, add an `mcp` key to `ma3-osc.json`:

```json
    "mcp": { "port": 9005 }
```

Then connect your agent, for example Claude Code:

```sh
claude mcp add --transport http ma3 http://127.0.0.1:9005/mcp
```

| Tool | What it returns |
|---|---|
| `ma_status` | bridge state (MA link, connected surfaces) and desk state (version, current page, command line, selection count, desk lock, masters) |
| `ma_list_executors` | the executors on a page, with the assigned object, current cue, run state and fader/key functions |
| `ma_list_pool` | number and name of the objects in a pool (sequences, groups, macros, presets, …), paged |
| `ma_list_fixtures` | the patch: FID/CID, name, fixture type, mode, DMX address, layer, class and stage, paged |

- All tools are read-only. The agent cannot change the show or the programmer.
- The server listens on 127.0.0.1 only and rejects requests whose Host or Origin header
  is not local.
- The tools need the current `plugin/ma3-osc.xml` running in grandMA3. If MA is
  unreachable, `ma_status` still reports the bridge side.

See [Agent access](docs/ARCHITECTURE.md#agent-access-mcp) in the manual for the full tool
reference and how requests reach MA over OSC.

## Documentation

The full manual is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). It covers MA setup,
configuration, the v2 mapping format, the message monitor, agent access (MCP) and
development notes.

## License

GPL-3.0, see [LICENSE](LICENSE).
