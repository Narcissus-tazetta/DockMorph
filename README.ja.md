# DockMorph

**どのアプリのアイコンも好きな画像に変えて、純正の Dock で動かせます。**

[English](README.md) · **日本語**

DockMorph は、他のアプリのアイコンを好きな画像(PNG・GIF など)に差し替える macOS のメニューバーアプリです。純正の Dock をそのまま使い、Dock の置き換えやアプリの署名の変更はしません。実験機能の「Dock で動かす」を使うと、アプリの起動中も Dock で GIF が動き続けます。

> [!WARNING]
> DockMorph は開発中です。Dock で動かす機能は Dock の未公開の挙動に依存するため、macOS のアップデートで動かなくなる可能性があります。動作を確認している環境は **macOS 27.2(Apple Silicon)** だけです。

## できること

- **アイコンの差し替え**: Dock・Finder・アプリ切り替えに表示されるアイコンを、好きな画像(PNG・GIF など)にします。画像は縦横比を保ったまま正方形に収めます。
- **自動で付け直す**: アプリの更新でアイコンが外れても、30 秒以内に付け直します。元のアプリが移動しても Bundle ID から追従します。
- **元に戻す**: 登録を外すと、アイコンも含めて完全に元の状態に戻ります。
- **設定画面**: 表示言語(自動・English・日本語)、ログイン時の起動、自動同期、アップデート、許可を設定できます。既定では、macOS の言語が日本語のときだけ日本語、それ以外は英語で表示します。
- **Dock で GIF を動かす(実験的)**: アプリ本物の Dock タイルで GIF を再生します。タイルはアプリごとに 1 つのままで、起動中の点も同じタイルに付きます。

## 仕組み

| 方式 | 内容 |
| --- | --- |
| in-place(既定) | 本物のバンドル直下に `Icon\r` を付けます。署名は有効なままです |
| clone(フォールバック) | 本物に書き込めないとき、APFS クローンの複製にアイコンを付けて複製を起動します。方式が切り替わったときは必ず警告します |

### Dock で GIF を動かす

起動中のアプリのタイルは Dock がアイコンを固定するため、本物のタイルは動かせません。そこで `~/Applications/DockMorph/<アプリ名>.app`(起動用アプリ)を作ります。

- 起動用アプリは Dock タイル用のプラグイン(`NSDockTilePlugIn`)で GIF を描きます。
- 本物が未起動なら、起動用アプリが本物の実行ファイルに `execv` します。プロセスは本物のまま(署名も本物)で、Dock のタイルは 1 つのままです。
- 本物のデータフォルダに `com.apple.macl` が付いているアプリは、`execv` だと壊れるため、本物を別に開く方式にします(警告が出ます)。
- 本物が直接開かれてタイルが分かれたときは、自動で終了して起動用アプリから開き直します(DockMorph の常駐中のみ)。
- 本物のリンク(URL スキーム)は、起動用アプリが受け取って本物へ渡します。
- Antigravity が起動中に元アイコンを指定し直す処理は、DockMorph の常駐中に自動で抑止します。本体ファイルと署名は変更しません。一時的にローカルの Electron デバッグ接続を使い、処理後に接続を閉じます。ポート 9229 が使用中のときや、アプリの更新で接続が無効になったときは警告を表示します。「今すぐ同期」で再試行できます。オフ・登録解除時は元の動作に戻ります。
- 本物が起動用アプリを通さずに動いたあと、Dock の右側(最近使ったアプリ)に残る本物のタイルは、自動で消します(DockMorph の常駐中のみ)。ユーザーがピン留めした本物のタイルは消しません。

#### アクセシビリティの許可

Dock のタイルのメニュー(オプション → Dock から削除 / Dock に残す)を操作して、Dock を再起動せずに動くタイルを置いたり外したりします。これには **アクセシビリティの許可** が要ります。

許可がないと Dock を再起動する方法に切り替わり、起動中のほかの動くタイルが分かれることがあります。

- Dock に置くとき、ほかの動くタイルが起動中なら、Dock を再起動せずに置きます。このときタイルは Dock が次に再起動するまで右側に出ます。ほかに起動中の動くタイルがなければ、Dock を再起動して正しい位置に置きます。
- オフにするとき、本物を元の位置に戻すには Dock の再起動が要ります。ほかの動くタイルが起動中なら再起動せず、本物は Dock に戻しません(警告が出ます)。

## インストール

配布は Apple Silicon のみです。

### Homebrew

```sh
brew install --cask narcissus-tazetta/tap/dockmorph
```

### 手動でインストール

[Releases](https://github.com/Narcissus-tazetta/DockMorph/releases) から `DockMorph-macos-vX.Y.Z.dmg` をダウンロードして開き、`DockMorph.app` を **アプリケーション** に移します。

### 初回起動の警告

DockMorph は公証されていないため、初回の起動を macOS が止めることがあります。開こうとしたあと、**システム設定 → プライバシーとセキュリティ** で DockMorph の **このまま開く** を選んでください。

### アップデート

配布版は [Sparkle](https://sparkle-project.org) で更新します。2 回目の起動で、自動で更新を確認するかどうかを聞きます。メニューバーのメニューの **アップデートを確認…** で、いつでも確認できます。更新は署名を検証してから適用します。Homebrew で入れた場合は `brew upgrade --cask dockmorph` でも更新できます。

配布版は ad-hoc 署名のため、更新のたびに別のアプリとして扱われ、アクセシビリティの許可が外れます。更新後は許可し直してください。

## 動作環境

- macOS 14 以降(アプリに設定している最低バージョンです。動作を確認しているのは macOS 27.2・Apple Silicon だけです)
- Rust(stable)、Xcode(`swiftc`・`codesign`・Metal Toolchain。GUI の gpui のビルドに必要)

## ビルド

配布はソースのみで、ビルド済みのリリースはありません。

```sh
git clone https://github.com/Narcissus-tazetta/DockMorph.git
cd DockMorph

# GUI(DockMorph.app)。出力は target/bundle/DockMorph.app
scripts/bundle.sh release

# CLI(target/release/dockmorph)
cargo build --release
```

`scripts/bundle.sh` はアプリ本体と起動用アプリの雛形だけをビルドします。CLI を使うときは `cargo build --release` も実行してください。

`scripts/bundle.sh` は、キーチェーンに Apple Development の証明書があればそれで署名します(`DOCKMORPH_SIGN_IDENTITY` で指定もできます)。この場合、ビルドし直してもアクセシビリティの許可は残ります。証明書がなければ ad-hoc 署名になり、ビルドし直すたびに許可が外れます。

## CLI

```
dockmorph add [--clone] <App.app> <image>   アイコンを付ける
dockmorph list                              登録と状態を表示
dockmorph sync                              元アプリの更新を検知して付け直す
dockmorph set-image <bundle-id> <image>     画像を差し替える
dockmorph animate <bundle-id> on <Launcher.app> [--no-pin]
dockmorph animate <bundle-id> off           Dock で動かす(実験的)
dockmorph rejoin <bundle-id>                分かれたタイルを 1 つに戻す
dockmorph upgrade-launchers <Launcher.app>  起動用アプリを新しい雛形に入れ替える
dockmorph clear-leftovers                   Dock に残った本物のタイル(起動していないもの)を消す
dockmorph refresh-dock                      Dock のアイコンキャッシュを消して再起動する
dockmorph launch <bundle-id>                起動
dockmorph remove <bundle-id>                アイコンを戻して登録を削除
```

`Launcher.app` は `DockMorph.app/Contents/Resources/Launcher.app` にある雛形です。

## データの場所

`~/Library/Application Support/DockMorph/` に登録(`entries.json`)・画像・Dock 設定のバックアップを置きます。環境変数 `DOCKMORPH_HOME` で変更できます。

## 構成

- `crates/core`: 登録・アイコンの付け外し・Dock の操作(`dockmorph-core`)
- `crates/cli`: CLI
- `crates/app`: GUI(gpui)。`launcher/` は起動用アプリの雛形(Swift)
- `scripts/`: バンドル作成とアプリアイコンの生成

## ライセンス

[MIT](LICENSE)
