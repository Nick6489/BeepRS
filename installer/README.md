# Packing BeepRS

Each release ships two packages of the same build: a portable zip (`files`) and an Inno installer zip (`inno`). Testers can unzip or run setup. 1.0.3 is the first mixed GitHub feed. 1.0.2 cannot check it.

The Inno script is per-user, `PrivilegesRequired=lowest`, AppId `{7B3E9C4A-1F62-4D8E-A5B1-90C2E84F6D17}`, directory `{localappdata}\Programs\BeepRS`. In the script, only the opening brace is doubled: `AppId={{7B3E9C4A-1F62-4D8E-A5B1-90C2E84F6D17}`. A second closing brace is stored in the AppId, the uninstall key will not match, and an installed copy cannot update. After install, the script deletes that mistyped key and any `unins*` files that are not the registered uninstaller. The script writes `beeprs-distribution` with the body `inno`. The portable zip must not contain that file.

## Tools

- Inno Setup 7: `winget install --id JRSoftware.InnoSetup.7 --exact`
- `FreshenInno.dll` (x64, matching `SetupArchitecture=x64`): rebuild from the current Freshen checkout with `.\inno\build-bridge.ps1`. This uses the static CRT; keep the bridge and `FreshenInno.iss` from the same checkout so installer progress counters match.
- `freshen-release`: `cargo build --release --locked --bin freshen-release` in `../freshen`

## Commands

Stage a portable tree (exe and `sounds/` only) and an installer tree (that plus `installer/beeprs-distribution`). Compile setup, rename it to `setup.exe` inside a payload folder, then:

```text
freshen-release pack portable-dist beeprs-1.0.6-x86_64-pc-windows-msvc.zip https://github.com/Nick6489/BeepRS/releases/download/v1.0.6/beeprs-1.0.6-x86_64-pc-windows-msvc.zip x86_64-pc-windows-msvc files beeprs 1.0.6 stable files.json
freshen-release pack inno-payload beeprs-1.0.6-inno.zip https://github.com/Nick6489/BeepRS/releases/download/v1.0.6/beeprs-1.0.6-inno.zip x86_64-pc-windows-msvc inno beeprs 1.0.6 stable inno.json
```

ISCC:

```text
ISCC /DAppVersion=1.0.6 /DSourceDir=... /DOutputDir=... /DFreshenInnoDll=...\FreshenInno.dll installer\BeepRS.iss
```

Insert notes, then merge `files.json` and `inno.json`, then sign. Keep publishing the mixed feed. 1.0.3 was the first one, and 1.0.2 still cannot check it.

Do not overwrite signed bytes. The private key is `keys/publisher.key`.
