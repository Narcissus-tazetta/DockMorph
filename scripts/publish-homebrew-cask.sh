#!/bin/zsh
# dist の zip から Homebrew cask を作り、Narcissus-tazetta/homebrew-tap の Casks/dockmorph.rb を更新する。
#   scripts/publish-homebrew-cask.sh X.Y.Z
# push には HOMEBREW_TAP_TOKEN(tap リポジトリへの書き込み権限)が要る。tap は他のアプリと共用なので README は触らない。
# zap に Application Support や ~/Applications/DockMorph を入れないのは、登録の記録を消すと、付けたアイコンや Dock のピン留めを戻せなくなるため。
set -euo pipefail
root=${0:A:h:h}
cd "$root"
version=${1:?version required (X.Y.Z)}
zip="dist/DockMorph-macos-v$version.zip"
tap_repo=Narcissus-tazetta/homebrew-tap
: ${HOMEBREW_TAP_TOKEN:?HOMEBREW_TAP_TOKEN is required}
[[ -f $zip ]] || { echo "zip not found: $zip" >&2; exit 1; }

sha256=$(shasum -a 256 "$zip" | cut -d' ' -f1)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git clone --depth 1 "https://x-access-token:${HOMEBREW_TAP_TOKEN}@github.com/${tap_repo}.git" "$work/tap"
mkdir -p "$work/tap/Casks"

cat > "$work/tap/Casks/dockmorph.rb" <<RUBY
cask "dockmorph" do
  version "$version"
  sha256 "$sha256"

  url "https://github.com/Narcissus-tazetta/DockMorph/releases/download/v#{version}/DockMorph-macos-v#{version}.zip"
  name "DockMorph"
  desc "Change app icons to your own images and animate them in the Dock"
  homepage "https://github.com/Narcissus-tazetta/DockMorph"

  depends_on macos: :sonoma
  depends_on arch: :arm64

  app "DockMorph.app"

  zap trash: [
    "~/Library/Preferences/app.dockmorph.DockMorph.plist",
    "~/Library/Saved Application State/app.dockmorph.DockMorph.savedState",
  ]

  caveats <<~EOS
    DockMorph is not notarized. If macOS blocks the first launch, open
    System Settings → Privacy & Security and choose "Open Anyway".
    Before uninstalling, remove your apps from DockMorph to restore their icons.
  EOS
end
RUBY

cd "$work/tap"
git config user.name github-actions
git config user.email github-actions@github.com
git add Casks/dockmorph.rb
if git diff --cached --quiet; then
  echo "tap は最新"
else
  git commit -m "chore(cask): dockmorph $version"
  git push origin HEAD
fi
