#!/bin/zsh
# 配布 zip に EdDSA 署名を付けて docs/appcast.xml を更新する(Sparkle 同梱の generate_appcast を使う)。
#   scripts/update-appcast.sh X.Y.Z
# 秘密鍵は環境変数 SPARKLE_PRIVATE_ED_KEY(CI)か、無ければ keychain のアカウント "dockmorph"(手元)から読む。
# リリースノートは release-notes/vX.Y.Z.md を更新ダイアログに埋め込む。
set -euo pipefail
root=${0:A:h:h}
cd "$root"
version=${1:?version required (X.Y.Z)}
tool="$(scripts/fetch-sparkle.sh)/../../../bin/generate_appcast"
zip="dist/DockMorph-macos-v$version.zip"
notes="release-notes/v$version.md"
appcast=${APPCAST_PATH:-docs/appcast.xml}
repo_url=https://github.com/Narcissus-tazetta/DockMorph
# 手元の検証で、配布先を GitHub 以外(ローカルの HTTP サーバー)に向けるための上書き
prefix=${DOWNLOAD_URL_PREFIX:-$repo_url/releases/download/v$version/}

for f in "$tool" "$zip" "$notes"; do
  [[ -e $f ]] || { echo "見つからない: $f" >&2; exit 1; }
done

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp "$zip" "$stage/"
# generate_appcast は拡張子違いの同名ファイルをそのアーカイブのリリースノートとして使う
cp "$notes" "$stage/DockMorph-macos-v$version.md"
# 既存の appcast があれば、それに項目を足す形で更新される
[[ -f $appcast ]] && cp "$appcast" "$stage/appcast.xml"

args=(--download-url-prefix "$prefix" --embed-release-notes --link "$repo_url" -o "$stage/appcast.xml")
if [[ -n ${SPARKLE_PRIVATE_ED_KEY:-} ]]; then
  printf '%s' "$SPARKLE_PRIVATE_ED_KEY" | "$tool" --ed-key-file - $args "$stage"
else
  "$tool" --account dockmorph $args "$stage"
fi

mkdir -p "${appcast:h}"
cp "$stage/appcast.xml" "$appcast"
echo "更新した: $appcast"
