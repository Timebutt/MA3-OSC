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

## Documentation

The full manual is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). It covers MA setup,
configuration, the v2 mapping format, the message monitor and development notes.

## License

GPL-3.0, see [LICENSE](LICENSE).
