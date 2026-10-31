# BeepRS

Beep, the audio game Liam Erven made small enough to explain in one sentence, now wearing a Windows dialog box and an updater with a clipboard.

A beep plays until you press Space. One of fourteen death sounds plays. The beep comes back. There is no timer, no score attack, and no plot. The score is how many aliens you have removed from the premises, and it is saved, because the updater is not allowed to eat it. The count is on the menu when you come back. The play window does not repeat it.

## Play

The menu is a dialog: a list, and buttons. New asks for a name. Play opens a window that does not contain a single control. That window listens to the keyboard.

| Key | What happens |
| --- | --- |
| Space | The beep stops, an alien dies, the score is written down, the beep returns. |
| Esc | The game window closes and the menu comes back. |

A new game plays `intro.opus` by itself. That file is the instruction manual. Space waits until it finishes, and then `bed.opus` and the beep start together. A saved game skips the lecture and starts with the bed and the beep.

## Download

The Windows build is a zip and an installer. Unzip the zip and run `beeprs.exe`, or run setup and let it put BeepRS in your user Programs folder. Both copies are the same game. They ask GitHub for different packages when they update. Installed copies of 1.0.3 and 1.0.4 have to run the installer. Update will not move them.

[BeepRS 1.0.5 for Windows](https://github.com/Nick6489/BeepRS/releases/tag/v1.0.5)

## What an update is allowed to replace

These eighteen files are permanent residents. Every later package has to bring them. The updater cannot fire one.

It may bring friends, if they stand in the `sounds/` hallway. A fifteenth death, a ruder beep, a longer lecture: a signed file under `sounds/` is welcome, and version 1.0 will install it. A file in the lobby, or a hand in the saves drawer, is still shown out.

| File | Role |
| --- | --- |
| `beeprs.exe` | The program, which is also its own helper. |
| `sounds/beep.opus` | The beep. It loops. |
| `sounds/bed.opus` | The music bed. |
| `sounds/intro.opus` | The introduction for a new game. |
| `sounds/die1.opus` | A death. |
| `sounds/die2.opus` | Another death. |
| `sounds/die3.opus` | A third death, for variety, or for spite. |
| `sounds/die4.opus` | A fourth death. |
| `sounds/die5.opus` | A fifth death. |
| `sounds/die6.opus` | A sixth death. |
| `sounds/die7.opus` | A seventh death. |
| `sounds/die8.opus` | An eighth death. |
| `sounds/die9.opus` | A ninth death. |
| `sounds/die10.opus` | A tenth death. |
| `sounds/die11.opus` | An eleventh death. |
| `sounds/die12.opus` | A twelfth death. |
| `sounds/die13.opus` | A thirteenth death. |
| `sounds/die14.opus` | A fourteenth death. |

The sounds are Opus because WAV was hauling around a lot of silence.

## What an update must leave alone

These are not in the signed portable package:

- `saves\*.json` for a zip copy, beside the program, one file per game, with the name and the alien count.
- `%LOCALAPPDATA%\BeepRS\saves\*.json` for an installed copy, so uninstalling the game does not throw out the aliens you already removed.
- `update-source.json`, beside the program, which is where this copy looks for a release.
- `beeprs-distribution`, which only the installer writes. The zip does not ship it. It tells this copy it was installed, so it asks for the Inno package.

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

This build calls itself `beeprs`, version `1.0.5`, channel `stable`, target `x86_64-pc-windows-msvc` when built with the usual Windows toolchain. The target string in the dialog is the one the binary was actually built for. Pack with that string, not with a guess. A zip copy asks for a `files` artifact. An installed copy asks for an `inno` artifact. The dialog says which copy you are.

The publisher public key is compiled in. The private key is `keys/publisher.key` on the machine that signs releases, and it is not in this repository. If you came here looking for it, the door is that way.

Install closes the program. A zip copy lets Freshen replace the files above and start the new build, which has to confirm that it actually started. An installed copy lets Freshen run the new setup after this process exits, then start the new build. Packing both packages is in [installer/README.md](installer/README.md).

The updater has a native progress bar. Download percentages come from Freshen's byte counts; verification and extraction use an animated bar. Installed copies open a separate native progress dialog before the game exits. It uses the same layout as Freshen's former native test app, and follows Freshen's authenticated Inno work counters. Without counters, or while finalizing, it stays indeterminate. Only the installer's actual successful exit establishes completion.

Close is disabled while installation is running. On success, choose **Open updated game**; a reboot-required result asks you to restart Windows first. A failed installer or a helper that disappears without a terminal result is reported without automatically retrying. If the progress dialog cannot become ready, BeepRS cancels the handoff and keeps the current game running. Disposable progress copies live under the user-private servicing directory, outside the installed files, and are cleared on a later normal launch once no longer running.

This UI ships in the new game executable: an older build still uses its old updater when installing this build. The new progress UI is available for subsequent updates initiated from this build. Portable copies keep their existing Freshen replacement and startup-confirmation behavior.

## Build

BeepRS expects the Freshen crate as a sibling directory, `../freshen`. This is a test host, not a polite library consumer.

```text
cargo run --bin beeprs
```

Building requires the Windows SDK resource compiler (automatically found under Windows Kits 10, or set `RC` to its path). The update dialogs are compiled `DIALOGEX` resources with Common Controls v6.

The installer script is `installer/BeepRS.iss`. It needs Inno Setup 7 and Freshen's `FreshenInno.dll`.

Windows is the platform this is being exercised on. The dialogs are ordinary Win32 dialogs. The game window is an ordinary overlapped window. Nobody was harmed in the making of a GPU.
