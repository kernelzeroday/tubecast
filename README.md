# tubecast

A small, fast CLI to cast YouTube videos to your TV from the terminal.

`tubecast` speaks the **YouTube Lounge API** — the same "Link with TV code"
mechanism the YouTube mobile app uses to control a TV. That makes it generic:
it works with [Playlet](https://github.com/iBicha/playlet), the native YouTube
app on Roku / smart TVs / consoles, and anything else that registers as a
YouTube "screen." Pair once, then cast and control playback from your shell.

```console
$ tubecast play "https://youtu.be/dQw4w9WgXcQ"
playing https://youtu.be/dQw4w9WgXcQ

$ tubecast status
Playing: https://youtu.be/dQw4w9WgXcQ [12/213s]
```

## Why

There wasn't a simple, device-agnostic YouTube caster on the command line.
Casting tools tend to be tied to one ecosystem (Chromecast, a specific app, or
a vendor remote API). The Lounge API is the lowest common denominator that
every YouTube-on-TV surface already implements, so a CLI built on it controls
whatever screen you have.

## Install

From source (requires a Rust toolchain):

```sh
git clone https://github.com/kernelzeroday/tubecast
cd tubecast
cargo install --path .
```

This puts `tubecast` on your `PATH` (`~/.cargo/bin`). Or just `cargo build
--release` and run `./target/release/tubecast`.

## Quick start

1. On the TV, open the YouTube-style app and find its pairing screen:
   - **Playlet:** Settings → **Link with TV code**
   - **Native YouTube app:** Settings → **Link with TV code**

   It shows a 12-digit code like `123 456 789 012`. The code rotates every few
   minutes, so pair promptly.

2. Pair (spaces are ignored):

   ```sh
   tubecast pair "123 456 789 012"
   ```

   The first device you pair becomes the default. Credentials are saved to a
   long-lived token, so you only pair once per TV.

3. Cast:

   ```sh
   tubecast play "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
   ```

## Commands

| Command | What it does |
| --- | --- |
| `pair <code> [--alias NAME] [--default]` | Pair with a TV using its code |
| `play <url\|id\|playlist>` | Cast a video or playlist (replaces current playback) |
| `add <url\|id>` | Add a video to the queue |
| `resume` / `pause` | Resume / pause playback |
| `next` / `prev` | Skip to next / previous video |
| `seek <seconds>` | Seek to a position |
| `volume <0-100>` | Set volume |
| `mute` / `unmute` | Toggle audio |
| `skip-ad` | Skip the current ad |
| `status [--timeout N]` | Show what's currently playing |
| `devices` | List paired TVs |

Anything that takes a target accepts a full URL, a `youtu.be` link, a
`/shorts`, `/embed`, or `/live` URL, a bare 11-character video id, or a playlist
id / `?list=` URL.

### Multiple TVs

Every paired TV gets an alias. Target a specific one with `--device`:

```sh
tubecast devices
# living-room (default)   Playlet on 50" TCL Roku TV
# bedroom                 YouTube on Bedroom TV

tubecast play "dQw4w9WgXcQ" --device bedroom
```

Pass `--default` to `pair` (or pick the only paired device automatically) to set
which TV commands hit when `--device` is omitted.

## How it works

The Lounge API is an undocumented, reverse-engineered protocol:

1. **Pair** — `POST /api/lounge/pairing/get_screen` exchanges the on-screen
   12-digit code for a `screenId` + `loungeToken`.
2. **Connect** — a `bind` request opens a long-poll session against the screen.
3. **Command** — playback commands (`setPlaylist`, `play`, `pause`, …) are
   POSTed into that session; the screen receives them over its own long-poll.

`tubecast` is a thin CLI over the [`youtube_lounge_rs`](https://crates.io/crates/youtube_lounge_rs)
crate, which implements the protocol. Control commands wait for the screen to
confirm the new state (and resend once if the first attempt isn't acknowledged),
so `pause`/`resume` land reliably instead of racing the connection teardown.

## Configuration

Paired devices and tokens live in:

```
~/.config/tubecast/config.json    # Linux
~/Library/Application Support/tubecast/config.json   # macOS
```

It's plain JSON — a `default_device` and a list of `devices`
(`alias`, `name`, `screen_id`, `lounge_token`). Lounge tokens are refreshed
automatically and written back when they expire.

## Troubleshooting

- **`pairing failed … 404`** — the code expired or was mistyped. Reopen the
  pairing screen to get a fresh code and pair again immediately.
- **`timed out waiting for the screen to connect`** — the TV is off, asleep, or
  the YouTube/Playlet app isn't open. Wake it and retry.
- **A control prints `(unconfirmed)`** — the command was sent but the screen
  didn't report the expected state in time. Usually it still applied; check
  `tubecast status`.

## Notes

This project is not affiliated with or endorsed by YouTube or Google. It relies
on an undocumented API that can change at any time.

## License

MIT — see [LICENSE](LICENSE).
