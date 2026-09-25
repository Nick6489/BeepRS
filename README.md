# BeepRS

Beep, the audio game Liam Erven made small enough to explain in one sentence, now wearing a Windows dialog box and an updater with a clipboard.

A beep plays until you press Space. One of three death sounds plays. The beep comes back. There is no timer, no score attack, and no plot. The score is how many aliens you have removed from the premises, and it is saved, because the updater is not allowed to eat it.

## Play

The menu is a dialog: a list, and buttons. New asks for a name. Play opens a window that does not contain a single control. That window listens to the keyboard.

| Key | What happens |
| --- | --- |
| Space | The beep stops, an alien dies, the score is written down, the beep returns. |
| Esc | The game window closes and the menu comes back. |

A new game plays `intro.opus` by itself. That file is the instruction manual. Space waits until it finishes, and then `bed.opus` and the beep start together. A saved game skips the lecture and starts with the bed and the beep.

## Download

The Windows build is a zip. Unzip it and run `beeprs.exe`. There is no installer. An installer would be a second program, and this one is already doing enough.

[BeepRS 1.0 for Windows](https://github.com/Nick6489/BeepRS/releases/tag/v1.0.0)

## What an update is allowed to replace

These seven files are permanent residents. Every later package has to bring them. The updater cannot fire one.

It may bring friends, if they stand in the `sounds/` hallway. A fourth death, a ruder beep, a longer lecture: a signed file under `sounds/` is welcome, and version 1.0 will install it. A file in the lobby, or a hand in the saves drawer, is still shown out.

| File | Role |
| --- | --- |
| `beeprs.exe` | The program, which is also its own helper. |
| `sounds/beep.opus` | The beep. It loops. |
| `sounds/bed.opus` | The music bed. |
| `sounds/intro.opus` | The introduction for a new game. |
| `sounds/die1.opus` | A death. |
| `sounds/die2.opus` | Another death. |
| `sounds/die3.opus` | A third death, for variety, or for spite. |

The sounds are Opus because WAV was hauling around a lot of silence.

## What an update must leave alone

These live beside the program and are not in the signed package:

- `saves\*.json`, one file per game, with the name and the alien count.
- `update-source.json`, which is where this copy looks for a release.

## Checking for an update

Update asks GitHub, specifically the releases of this repository, for a signed stable release newer than the one you are running. The dialog stays up while that happens. That is why the dialog exists.

`update-source.json`, beside the program, is optional. It is kept when the program updates. If it is there, it overrides GitHub. That file is for packing a test release on a desk.

Two shapes work:

```json
{"type":"directory","path":"C:\\path\\to\\release"}
```

```json
{"type":"manifest","manifest":"https://example/freshen-manifest.json","signature":"https://example/freshen-manifest.json.sig"}
```

A directory release contains `freshen-manifest.json`, `freshen-manifest.json.sig`, and the zip. The zip's file name is the last part of the URL stored in the manifest.

This build calls itself `beeprs`, version `1.0.0`, channel `stable`, target `x86_64-pc-windows-msvc` when built with the usual Windows toolchain. The target string in the dialog is the one the binary was actually built for. Pack with that string, not with a guess.

The publisher public key is compiled in. The private key is `keys/publisher.key` on the machine that signs releases, and it is not in this repository. If you came here looking for it, the door is that way.

Install closes the program. Freshen's helper then replaces the files above and starts the new build. The new build has to confirm that it actually started. If it never does, the helper keeps the evidence and can roll the installation back.

## Build

BeepRS expects the Freshen crate as a sibling directory, `../freshen`. This is a test host, not a polite library consumer.

```text
cargo run --bin beeprs
```

Windows is the platform this is being exercised on. The dialogs are ordinary Win32 dialogs. The game window is an ordinary overlapped window. Nobody was harmed in the making of a GPU.
