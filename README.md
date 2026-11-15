# DockMorph

純正の Dock を使ったまま、他のアプリのアイコンを好きな画像(PNG・GIF など)に変える macOS アプリです。メニューバーに常駐し、アプリの更新でアイコンが外れても自動で付け直します。

## できること

- **アイコンの差し替え**: 登録したアプリのアイコンを、Dock・Finder・アプリ切り替えで好きな画像にします。画像は縦横比を保ったまま正方形に収めます。
- **自動で付け直す**: アプリの更新でアイコンが外れても、30 秒以内に付け直します。元のアプリが移動しても Bundle ID から追従します。
- **元に戻す**: 登録を外すと、アイコンも含めて完全に元の状態に戻ります。
- **Dock で GIF を動かす(実験的)**: 動くタイルを持つ「起動用アプリ」を作り、そこから本物を開くことで、Dock の GIF を動かします。

## 仕組み

| 方式 | 内容 |
| --- | --- |
| in-place(既定) | 本物のバンドル直下に `Icon\r` を付けます。署名は有効なままです |
| clone(フォールバック) | 本物に書き込めないとき、APFS クローンの複製にアイコンを付けて複製を起動します。方式が切り替わったときは必ず警告します |

### Dock で GIF を動かす

本物のタイルは Dock がアイコンを固定するため動かせません。そこで `~/Applications/DockMorph/<アプリ名>.app`(起動用アプリ)を作ります。

- 起動用アプリは Dock タイル用のプラグイン(`NSDockTilePlugIn`)で GIF を描きます。
- 本物が未起動なら、起動用アプリが本物の実行ファイルに `execv` します。プロセスは本物のまま(署名も本物)で、Dock のタイルは 1 つのままです。
- 本物のデータフォルダに `com.apple.macl` が付いているアプリは、`execv` だと壊れるため、本物を別に開く方式にします(警告が出ます)。
- 本物が直接開かれて動かないタイルが分かれたときは、自動で終了して起動用アプリから開き直します(DockMorph の常駐中のみ)。
- 本物のリンク(URL スキーム)は、起動用アプリが受け取って本物へ渡します。
- Antigravity が起動中に元アイコンを指定し直す処理は、DockMorph の常駐中に自動で抑止します。本体ファイルと署名は変更しません。一時的にローカルの Electron デバッグ接続を使い、処理後に接続を閉じます。ポート 9229 が使用中のときや、アプリの更新で接続が無効になったときは警告を表示します。「今すぐ同期」で再試行できます。オフ・登録解除時は元の動作に戻ります。
- Dock のタイルのメニュー(オプション → Dock から削除 / Dock に残す)を操作して、Dock を再起動せずに動くタイルを外します。これには **アクセシビリティの許可** が要ります。許可がないと Dock を再起動する方法に切り替わり、起動中のほかの動くタイルが分かれることがあります。
- Dock に置くとき、ほかの動くタイルが起動中なら、Dock を再起動せずに置きます。このときタイルは Dock が次に再起動するまで右側に出ます。ほかに起動中の動くタイルがなければ、Dock を再起動して正しい位置に置きます。
- オフにするとき、本物を元の位置に戻すには Dock の再起動が要ります。ほかの動くタイルが起動中なら再起動せず、本物は Dock に戻しません(警告が出ます)。
- 本物が起動用アプリを通さずに動いたあと、Dock の右側(最近使ったアプリ)に残る本物のタイルは、自動で消します(DockMorph の常駐中のみ。アクセシビリティの許可が要ります)。ユーザーがピン留めした本物のタイルは消しません。
- `scripts/bundle.sh` は、キーチェーンに Apple Development の証明書があればそれで署名します(`DOCKMORPH_SIGN_IDENTITY` で指定もできます)。この場合、ビルドし直してもアクセシビリティの許可は残ります。証明書がなければ ad-hoc 署名になり、ビルドし直すたびに許可が外れます。

## 動作環境

- macOS 14 以降
- Rust(stable)、Xcode(`swiftc`・`codesign`・Metal Toolchain。GUI の gpui のビルドに必要)

## ビルド

```sh
# GUI(DockMorph.app)。出力は target/bundle/DockMorph.app
scripts/bundle.sh release

# CLI(target/release/dockmorph)
cargo build --release
```

`scripts/bundle.sh` はアプリ本体と起動用アプリの雛形だけをビルドします。CLI を使うときは `cargo build --release` も実行してください。

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
