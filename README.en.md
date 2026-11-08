# Serein with a built-in DPI bypass

Fork of [Serein](https://github.com/ViceVerse-cz/Serein), a native Discord client written in Rust (egui/wgpu), with the [zapret](https://github.com/bol-van/zapret) DPI bypass wired into the app. Strategy list comes from [Flowseal's Windows bundle](https://github.com/Flowseal/zapret-discord-youtube).

The original client cannot get past network filtering, so in Russia it simply fails to connect. This fork brings the bypass up before the client tries to sign in.

## What is added

A new page in Settings, "DPI bypass", with two modes.

Auto walks the strategies in order and stops at the first one after which Discord actually answers. The next launch starts from the same strategy, so normally that is one probe instead of a full sweep.

Manual runs every strategy and lists the results. Working rows are highlighted; clicking one selects it.

Also:

- on startup the app checks whether a bypass is already running, including the case where zapret is installed as a Windows service, and does not start a second copy;
- if nothing is running, the bypass starts together with the client;
- `winws` runs elevated through a scheduled task that is registered once during installation, so no UAC prompts afterwards;
- the runner is a VBS script executed by `wscript`, so no console windows appear;
- the working strategy is saved and stays connected after the scan.

## How it works

A scheduled task, SereinZapret, starts a generated script with the highest privileges; the script launches `zapret\bin\winws.exe` with the arguments of the chosen strategy. A second task, SereinZapretStop, kills `winws` when the bypass is turned off: `schtasks /end` terminates only the host process, the child keeps running.

Strategy arguments live in `crates/zapret/data/strategies.json`. The catalog is generated from Flowseal's `general*.bat` files by `tools/gen-strategies.ps1`.

A strategy counts as working when at least two of three Discord endpoints answer: `discord.com/api/v10/gateway`, `gateway.discord.gg`, `cdn.discordapp.com`. Any HTTP status counts, including 403 from the CDN, because it means TLS and the request went through.

## Installation

Installer: `serein-<version>-setup.exe`. It installs into the user profile and asks for administrator rights once, to register the scheduled tasks.

Portable build: unpack the archive and run `serein.exe`. On the first launch the app requests elevation itself and registers the tasks.

The login is taken from the Windows Credential Manager, so an existing installation carries over.

## Updates

`.github/workflows/sync-upstream.yml` merges upstream `main` into the fork once a day.

`.github/workflows/refresh-zapret.yml` downloads the latest Flowseal release once a day, regenerates the strategy catalog and refreshes the binaries in `packaging/windows/zapret`.

`.github/workflows/fork-build.yml` builds the installer and the portable archive on every push; releases are published on `v*` tags.

## Licenses

Serein is MIT or Apache-2.0. zapret and Flowseal's bundle are MIT. WinDivert is LGPL-3.0 or GPL-2.0. `cygwin1.dll` is GPL-3.0 with the linking exception. Full texts and source links are in `packaging/windows/zapret/NOTICE.txt`.

Thanks to bol-van for zapret, to Flowseal for the strategy bundle, and to the WinDivert authors.

## Legal note

Not affiliated with Discord. Using a third-party client with a normal account violates Discord's Terms of Service and can get the account terminated. In Russia personal use of circumvention tools is not prohibited, while advertising and promoting them is an administrative offence since 1 September 2025. Judge the risks before publishing a build.
