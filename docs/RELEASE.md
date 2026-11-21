# Release procedure

Releases are built by the `release` GitHub Actions workflow and distributed as an ad-hoc signed, non-notarized app for Apple Silicon. Updates are delivered with [Sparkle](https://sparkle-project.org); Homebrew users get the same zip through the `narcissus-tazetta/tap` cask.

## What a release produces

| Output | Purpose |
| --- | --- |
| `DockMorph-macos-vX.Y.Z.zip` | Sparkle update archive and Homebrew cask download |
| `DockMorph-macos-vX.Y.Z.dmg` | Manual download |
| `docs/appcast.xml` | Sparkle feed, committed to `main` and read from `raw.githubusercontent.com` |
| `Casks/dockmorph.rb` in `Narcissus-tazetta/homebrew-tap` | Homebrew cask |

The app reads its feed from `SUFeedURL` and verifies updates against `SUPublicEDKey` (`scripts/sparkle-public-key.txt`). Both are written into `Info.plist` only when `scripts/bundle.sh` runs with `DOCKMORPH_VERSION`; other builds have no update source and show the update item as unavailable. The repository and its releases must be public for clients to fetch the feed and archives.

## One-time setup

1. **Sparkle key.** The private key lives in the login keychain under the account `dockmorph`. Back it up and keep it out of the repository; losing it means existing installs can no longer receive updates.

    ```sh
    scripts/fetch-sparkle.sh   # downloads Sparkle into target/sparkle if missing
    target/sparkle/2.10.1/bin/generate_keys --account dockmorph -x ~/dockmorph-sparkle-private.key
    ```

2. **Repository secrets** (Settings → Secrets and variables → Actions):

    | Secret | Value |
    | --- | --- |
    | `SPARKLE_PRIVATE_ED_KEY` | Contents of the exported private key file |
    | `HOMEBREW_TAP_TOKEN` | Token with write access to `Narcissus-tazetta/homebrew-tap`. Optional: without it the workflow skips the cask update and shows a warning |

    ```sh
    gh secret set SPARKLE_PRIVATE_ED_KEY < ~/dockmorph-sparkle-private.key
    ```

## Releasing

1. Write `release-notes/vX.Y.Z.md`. It becomes the GitHub release body and the text in Sparkle's update dialog, so describe only what users notice, one line per item.
2. Run the **release** workflow (Actions → release → Run workflow) with the version.
3. The workflow builds, signs the appcast, creates the GitHub release, updates the cask, and then commits `docs/appcast.xml`. The appcast is pushed last so that no client is pointed at an archive that does not exist yet.

The build number Sparkle compares is derived from the version (`major * 10000 + minor * 100 + patch`, see `scripts/derive_build_number.sh`); minor and patch must be 99 or less.

## Building locally

Install the DMG packaging tool with `brew install create-dmg`.

```sh
scripts/package.sh X.Y.Z                 # dist/DockMorph-macos-vX.Y.Z.{zip,dmg}
scripts/update-appcast.sh X.Y.Z          # signs with the keychain key and updates docs/appcast.xml
```

`SPARKLE_FEED_URL` at package time and `DOWNLOAD_URL_PREFIX` / `APPCAST_PATH` at appcast time point the feed at a local HTTP server for testing an update end to end.
