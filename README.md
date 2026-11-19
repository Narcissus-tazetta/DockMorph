# DockMorph

**Change any app's icon to your own image, and make it animate in the native Dock.**

**English** · [日本語](README.ja.md)

DockMorph is a macOS menu bar app that replaces other apps' icons with your own PNG or GIF. It keeps the stock Dock: no Dock replacement and no changes to the apps' signatures. With the optional animated Dock tile, a GIF keeps moving in the Dock while the app is running.

> [!WARNING]
> DockMorph is under development. The animated Dock tile relies on undocumented behavior of the Dock, so a macOS update may break it. The tested environment is **macOS 27.2 on Apple Silicon**. The interface is available in English and Japanese.

## Features

- **Replace icons** shown in the Dock, Finder, and app switcher with a PNG, GIF, or other image. Images keep their aspect ratio and are fitted into a square.
- **Reapply automatically** when an app update removes the icon (within 30 seconds), and follow the app if it moves, by Bundle ID.
- **Restore completely.** Unregistering an app brings back the original icon.
- **Settings screen** for language (automatic, English, or Japanese), launch at login, automatic sync, updates, and permissions. By default, DockMorph shows Japanese when your macOS language is Japanese and English otherwise.
- **Animate the Dock tile (experimental).** A GIF plays in the app's real Dock tile, with one tile per app and the running indicator on the same tile.

## How it works

| Method | What it does |
| --- | --- |
| In-place (default) | Attaches the custom icon (`Icon\r`) to the app bundle. The app's code signature stays valid |
| Clone (fallback) | When the bundle is not writable, attaches the icon to an APFS clone of the app and launches the clone. DockMorph always warns when it switches to this method |

### Animated Dock tile

The Dock freezes a running app's tile image, so the real app's tile cannot be animated. DockMorph instead creates a launcher app at `~/Applications/DockMorph/<App name>.app`.

- The launcher draws the GIF through a Dock tile plug-in (`NSDockTilePlugIn`).
- If the real app is not running, the launcher replaces itself with the real app's executable using `execv`. The process is the real app, signed as the real app, and the Dock shows a single tile.
- Apps whose data folders carry `com.apple.macl` break under `execv`. For these, DockMorph opens the real app separately instead and shows a warning.
- When the real app is opened without the launcher and the tile splits, DockMorph tells you in Notification Center, its window, and the menu bar menu (while DockMorph is running). It never quits the app on its own, so documents and arguments the app was opened with are not lost. **Rejoin** quits the app and reopens it through the launcher.
- URL schemes of the real app are handled by the launcher and forwarded to it.
- Antigravity resets its own icon while running. While DockMorph is running, it suppresses this without modifying the app's files or signature. It temporarily uses a local Electron debugging connection and closes it afterward. If port 9229 is in use or an app update disables the connection, DockMorph shows a warning, and **Sync now** retries. Turning the feature off or unregistering the app restores the original behavior. While the connection is briefly open, other processes of the same user can also connect to Antigravity. If that concerns you, turn off **Keep the icon of the running Antigravity** in Settings.
- Leftover tiles of the real app in the Dock's recent-apps section are removed automatically (while DockMorph is running). Tiles you pinned yourself are never removed.

#### Accessibility permission

DockMorph operates the Dock tile menus (Options → Remove from Dock / Keep in Dock) to add and remove animated tiles without restarting the Dock. This requires **Accessibility** permission.

Without it, DockMorph falls back to restarting the Dock. A restart can split the tiles of other running animated apps.

- When placing a tile while another animated app is running, DockMorph does not restart the Dock. The new tile appears on the right side until the Dock next restarts. With no other animated app running, DockMorph restarts the Dock and places the tile in the correct position.
- When turning the feature off, restoring the real app's original Dock position requires a Dock restart. If another animated app is running, DockMorph skips the restart, leaves the real app out of the Dock, and shows a warning.

## Install

Releases support Apple Silicon only.

### Homebrew

```sh
brew install --cask narcissus-tazetta/tap/dockmorph
```

### Manual installation

Download `DockMorph-macos-vX.Y.Z.dmg` from [Releases](https://github.com/Narcissus-tazetta/DockMorph/releases), open it, and move `DockMorph.app` to **Applications**.

### First-launch warning

DockMorph is not notarized, so macOS may block the first launch. After trying to open it, choose **Open Anyway** for DockMorph in **System Settings → Privacy & Security**.

### Updating

Released builds update through [Sparkle](https://sparkle-project.org). On the second launch, DockMorph asks whether to check for updates automatically, and **Check for Updates…** in the menu bar menu checks on demand. Each update's signature is verified before it replaces the app. Homebrew installations can also use `brew upgrade --cask dockmorph`.

Releases are ad-hoc signed, so macOS treats each update as a different app and revokes the Accessibility permission. Grant it again after updating.

## Uninstall

1. Choose **Settings → Restore Everything and Quit**. DockMorph restores the original icons of all registered apps, removes the animated Dock tiles and launcher apps, removes itself from Login Items, and then quits (`dockmorph remove-all` on the CLI).
2. Move `DockMorph.app` to the Trash (or run `brew uninstall --cask dockmorph`).
3. To delete registrations and images, also delete `~/Library/Application Support/DockMorph/`.

If you delete DockMorph.app without step 1, the custom icons, the launcher apps pinned to the Dock, and the link handlers the launchers took over stay behind. Reinstall DockMorph and do step 1 to clean them up.

## Requirements

- macOS 14 or later (the minimum version set in the app bundle; only macOS 27.2 on Apple Silicon has been tested)
- Rust (stable) and Xcode, which provides `swiftc`, `codesign`, and the Metal Toolchain that gpui needs

## Build and run

DockMorph is distributed as source only. There are no prebuilt releases.

```sh
git clone https://github.com/Narcissus-tazetta/DockMorph.git
cd DockMorph

# GUI. Output: target/bundle/DockMorph.app
scripts/bundle.sh release

# CLI. Output: target/release/dockmorph
cargo build --release
```

`scripts/bundle.sh` builds only the app and the launcher template, so run `cargo build --release` as well when you use the CLI.

`scripts/bundle.sh` signs with an Apple Development certificate from your keychain when one is available; set `DOCKMORPH_SIGN_IDENTITY` to choose one. Accessibility permission then survives rebuilds. Without a certificate, the app is signed ad hoc and macOS revokes the permission on every rebuild.

## CLI

```
dockmorph add [--clone] <App.app> <image>   Attach an icon
dockmorph list                              Show registrations and their state
dockmorph sync                              Detect app updates and reapply icons
dockmorph set-image <bundle-id> <image>     Replace the image
dockmorph animate <bundle-id> on <Launcher.app> [--no-pin]
dockmorph animate <bundle-id> off           Animated Dock tile (experimental)
dockmorph rejoin <bundle-id>                Merge a split tile back into one
dockmorph upgrade-launchers <Launcher.app>  Update launchers to the current template
dockmorph clear-leftovers                   Remove leftover real-app tiles that are not running
dockmorph refresh-dock                      Clear the Dock icon cache and restart the Dock
dockmorph launch <bundle-id>                Launch the app
dockmorph remove <bundle-id>                Restore the icon and unregister the app
dockmorph remove-all                        Restore every registered app (use before uninstalling)
```

`Launcher.app` is the template at `DockMorph.app/Contents/Resources/Launcher.app`.

## Data location

Registrations (`entries.json`), images, and Dock settings backups live in `~/Library/Application Support/DockMorph/`. Set the `DOCKMORPH_HOME` environment variable to change the location.

## Repository layout

- `crates/core`: registrations, icon attach/detach, and Dock operations (`dockmorph-core`)
- `crates/cli`: the CLI
- `crates/app`: the GUI (gpui). `launcher/` holds the launcher app template (Swift)
- `scripts/`: bundle creation and app icon generation

## License

[MIT](LICENSE)
