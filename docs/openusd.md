# OpenUSD / Hugging Face SimReady

実装・確認日: 2026-10-23。

## 使い方

「ファイル → OpenUSDを読み込む…」から、ローカルファイルかHugging Faceの公開パッケージを読み込む。現在の文書へ追加し、読み込み後はアセット全体へカメラを向ける。1回のUndoで追加した全メッシュを取り消せる。

- **ローカルUSD**: `.usd`、`.usda`、`.usdc`、`.usdz`を選択。外部参照がある場合は元のフォルダ構成も必要。
- **Hugging Face**: SimReady Centralでアセットを見つけ、データセット内の `com.nvidia.simready.packaging.json` の `resolve` または `blob` URLを貼り付ける。SpaceのURL自体や、単独のUSDダウンロードURLはパッケージURLではない。
- **保存**: メッシュを含む `.u3d` はv3として保存。取り込んだ静止形状・代表色・出典を内包するため、再表示時にUSD SDKやネット接続は不要。v1／v2も読み込める。

検証したパッケージ:

```text
https://huggingface.co/datasets/nvidia/simready-assets/resolve/524747d9b5e1f21b5b18b56041788c01d745f1a1/IV_pole_a01/com.nvidia.simready.packaging.json
```

公開資料: [SimReady Central](https://huggingface.co/spaces/nvidia/simready-central)、[NVIDIAの利用・配布仕様](https://docs.omniverse.nvidia.com/simready/latest/simready-central.html)。同仕様では、アセットの参照・素材・ライセンスとデータセットrevisionを保持して利用することが説明されている。

## OpenUSDランタイム

USDの構文・バイナリ・参照合成は公式 `usd-core==26.11` のSDKで処理する。PythonはSDKへの変換アダプター専用。Document、履歴、三角形分割、メッシュ保存、GPU資源と描画はRust／wgpuが担当する。WebViewへ頂点列や画像フレームを送らない。

開発環境ではPython 3.11〜3.14を指定して実行する。

```sh
PYTHON=/path/to/python3.12 sh scripts/setup-usd.sh
```

このワークスペースには `.usd-runtime` を準備済み。別のランタイムを使う場合は、`usd-core`を導入したPython実行ファイルの絶対パスを環境変数 `UNIVERSAL3D_USD_PYTHON` に指定する。検出順は環境変数、開発ワークスペースの `.usd-runtime`、PATH上のPython。ランタイムがなければ取り込み画面に準備方法を表示する。

今回のデバッグappにPython／SDKそのものは同梱していない。開発ディレクトリを持たない配布先へのランタイム同梱、署名・公証、Windows/LinuxでのSDK起動は未検証。

## 対応範囲

| 項目 | 現在の動作 |
| --- | --- |
| レイヤー・参照・ペイロード | SDKで合成。解決できない合成はエラー。オーサリング済みのvariant選択を利用 |
| 階層Transform | ワールド変換を静止形状へ焼き込み、各メッシュを独立オブジェクトとして配置。元のprim pathを保存 |
| 単位・軸 | `metersPerUnit`を使ってメートルへ変換。Y-up／Z-upをアプリのY-upへ変換 |
| ポリゴン | 三角形・四角形・凹多角形を三角形化。hole faceを除外し、orientationを反映。非単純／不正トポロジは拒否 |
| 表示・インスタンス | 継承visibilityを評価。guide／proxy purposeは除外。ネイティブUSDインスタンスはinstance proxyを走査して静止化 |
| 色 | displayColorの先頭値または未接続のUsdPreviewSurface diffuseColorを代表色として使用 |
| 編集 | 選択・Fフォーカス・移動／回転／スケール・シアー・ミラー・表示切替・削除・Undo／Redo |
| 診断 | 件数、三角形数、出典、コミット、ライセンス、近似／省略した機能を表示 |

テクスチャ、MDL／MaterialXの評価、PBR、頂点／面ごとの色、スムーズ法線、サブディビジョン評価は未対応。フラット陰影の静止ポリゴン表示になる。アニメーションは開始時刻（時間範囲がなければdefault）で評価し、タイムラインとは連動しない。PointInstancer、カーブ、USD解析プリミティブは省略を診断表示する。物理・リグ・セマンティクスを本アプリのシミュレーション機能として実行しない。USDステージの編集やUSDへの書き戻しも未実装。

## 取得とリソース管理

公開HFデータセットのパッケージに限定する。ブランチ名をコミットSHAに解決してからBOMを取得し、BOMと各記載ファイルのSHA-256・サイズを確認する。相対パスの逸脱・重複、不正なルート、対応外バージョンを拒否する。ライセンス、依存USD、テクスチャ等を含む元パッケージは、OSのアプリキャッシュディレクトリの `openusd/package-*` に保持する。現在は取り込みごとに取得するため、自動キャッシュ再利用・容量回収は今後の作業。

複数のルートUSDを持つパッケージは自動選択せず、ローカルへ取得した上で対象USDを選ぶよう案内する。認証が必要なprivate／gatedデータセットは未対応。

初期上限はパッケージ512 MiB／2,048ファイル、1回1,000メッシュ、変換中間ファイル64 MiB、Document全体で500,000三角形／1,000メッシュ資源。旧オブジェクト上限10,000とTransform範囲も維持する。Undo／Redoの参照先を壊さないようメッシュ資源は保持し、保持済み資源も予算に含める。容量を超えたアセットは縮小せずエラーにする。

取得・SDK実行は別プロセス、Rust側のワーカースレッドで待機する。取消または5分の制限時間でSDKプロセスを停止する。文書に追加する直前にも取消を確認し、不完全な変換を追加しない。変換成功後に一括で文書へ追加し、GPUバッファは必要なメッシュのみ作る。カメラのみの変更ではメッシュを再転送しない。

2026-10-30：変換したメッシュの頂点・エッジ・面スナップ用索引も、Editorのロックを取得する前にワーカー内で準備する。頂点重複除去・Morton並べ替え・エッジ再構成・境界生成・BVH構築の途中で取消を確認し、取消時は途中の索引と未確定メッシュを破棄する。文書への一括追加が成功した後、不変アセットのArc同一性で索引を採用する。取り込み完了までの処理時間には準備時間が加わる。保存形式・IPCに索引は含めない。

`.u3d`内の静止メッシュは外部キャッシュに依存しない。元の完全なSimReadyパッケージを別環境で再利用する場合は、出典の固定revisionから再取得するか、ライセンスと依存ファイルを含めて元パッケージを保管する。

## 検証

- NVIDIA `IV_pole_a01`（上記固定revision）を公開HFから取得し、BOM／ファイルのハッシュ検証、公式SDK合成、16メッシュ・458,714三角形への変換に成功。
- 実アセットをMetalでオフスクリーン描画し、GPU画像を読み戻して形状が描画されていることを確認。`.u3d` v3の保存・再読込と一括Undo／Redoも確認。
- SDK子プロセス起動と取消、USDC参照・USDZ・cm/Z-up・親Transform・visibility・instance proxy、欠落参照拒否を検証。
- 凹多角形、欠損メッシュID、失敗時の原子性、履歴の資源共有、メッシュ情報をIPCに含めないこと、メッシュのピッキングとGPUバッチ更新をRustで検証。
- Chromiumで取り込み画面の日本語・英語・简体中文、1280／375px、成功・診断・SDK不足・Escape取消を確認。これは実コンポーネント＋模擬IPCによるUI試験。
- macOSデバッグappのビルド成功。ネイティブのファイルダイアログから実アセットを取り込み、中央Metalビューポートの表示、一括Undoで17→1オブジェクト、Redoで1→17オブジェクトへの復元を確認。Windows/Linux、配布先ランタイムはPending。

再実行:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --offline
.usd-runtime/bin/python -m unittest discover -s tests -p usd_import_test.py
# 実データの中間JSONを取得済みの場合
U3D_USD_FIXTURE=/path/to/mesh.json cargo test --manifest-path src-tauri/Cargo.toml gpu_real_simready_mesh -- --ignored
```


## エッジ・面スナップ（2026-10-30）

新しい取り込みでは、三角分割前の各ポリゴン境界を三角形の辺01／12／20へ対応させる。`.u3d`のMeshに任意の`edge_mask`文字列を保存し、各ASCII文字0〜7の3ビットで辺の有無を表す。1三角形1バイトのため、大規模メッシュで境界座標やJSONの真偽値配列を重複保存しない。凹ポリゴン・left-handedの反転後の頂点順でも対応を検証する。既存のv3メッシュはマスクなしでも読み込める。

マスクがあれば同一平面の隣接ポリゴン辺も吸着対象となり、三角分割で生成した対角線を除外できる。マスクのない既存メッシュ・プリミティブは三角形の境界・折れ目・非多様体辺から再構成するため、失われた元の平面内ポリゴン辺を完全には復元できない。面スナップは保存された静止三角形の表面を使う。元のn-gon単位の面中心や面法線への回転合わせではない。
