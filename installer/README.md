# Packing BeepRS 1.0.3

1.0.3 ships two packages of the same build: a portable zip (`files`) and an Inno installer zip (`inno`). Testers can unzip or run setup. 1.0.4 is the mixed-feed update that exercises Freshen for both copies.

The Inno script is per-user, `PrivilegesRequired=lowest`, AppId `{7B3E9C4A-1F62-4D8E-A5B1-90C2E84F6D17}`, directory `{localappdata}\Programs\BeepRS`. It writes `beeprs-distribution` with the body `inno`. The portable zip must not contain that file.

## 1.0.2 Update

1.0.2 still calls Freshen's legacy `check`. A mixed `files`+`inno` manifest on GitHub makes that call fail. For the 1.0.3 GitHub release, sign a **files-only** manifest so 1.0.2 can still zip-update, and attach `BeepRS-1.0.3.exe` as an extra asset for installer testers. 1.0.3 itself uses `check_for_kind`. Tomorrow's 1.0.4 is the first mixed feed.

Local desk tests can merge both artifacts and point `update-source.json` at that folder.

## Tools

- Inno Setup 7: `winget install --id JRSoftware.InnoSetup.7 --exact`
- `FreshenInno.dll` (x64): from the Freshen checkout, `.\inno\build-bridge.ps1`
- `freshen-release`: `cargo build --release --locked --bin freshen-release` in `../freshen`

## Commands

Stage a portable tree (exe and `sounds/` only) and an installer tree (that plus `installer/beeprs-distribution`). Compile setup, rename it to `setup.exe` inside a payload folder, then:

```text
freshen-release pack portable-dist beeprs-1.0.3-x86_64-pc-windows-msvc.zip https://github.com/Nick6489/BeepRS/releases/download/v1.0.3/beeprs-1.0.3-x86_64-pc-windows-msvc.zip x86_64-pc-windows-msvc files beeprs 1.0.3 stable files.json
freshen-release pack inno-payload beeprs-1.0.3-inno.zip https://github.com/Nick6489/BeepRS/releases/download/v1.0.3/beeprs-1.0.3-inno.zip x86_64-pc-windows-msvc inno beeprs 1.0.3 stable inno.json
```

ISCC:

```text
ISCC /DAppVersion=1.0.3 /DSourceDir=... /DOutputDir=... /DFreshenInnoDll=...\FreshenInno.dll installer\BeepRS.iss
```

Insert notes, then sign. For GitHub 1.0.3, sign the files manifest only. Merge `files.json` and `inno.json` when you want a mixed feed for 1.0.4 or a local test.

Do not overwrite signed bytes. The private key is `keys/publisher.key`.
