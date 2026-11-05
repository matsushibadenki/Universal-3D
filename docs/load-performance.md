# 実シーンの読込全工程計測

2026-11-05。🟢 [Done] 本番のCPU読込経路を新しい専用プロセスで再実行し、工程別wall timeと対象プロセス／SDK子プロセスのRSSをJSONへ記録する。ThreeXのジョブ診断と保持容量を分ける設計を参考に、独自に実装した。リンク・共有クレート・コード流用への依存は追加していない。

## 計測範囲

- USD: ローカルの実USDのSDK起動・参照合成・抽出、中間JSONのI/O／解析、三角形化、文書検証、頂点／エッジ／面索引、配置木、追記採用。
- Open: 自己完結した.u3dのI/O／解析、文書検証、形状／配置索引の構築、採用。
- Recovery: 同じ実シーンを保存した復旧コピーのI/O／解析・検証・索引準備、専用一時ディレクトリへのチェックポイント再保存、未保存コピーとしての採用。入力候補を削除／消費しない。
- 読込完了後の索引取得、固定画面位置での最初のエッジ／面問い合わせも別記録する。読込時間本体には含めない。カメラは経路の既存動作に従い、経路間で問い合わせ位置の幾何条件を正規化していない。hit有無を必ず記録する。

**ヘッドレスのCPU経路**を対象とする。ファイル選択・未保存確認の人間待ち時間、起動時の復旧候補探索、Tauri通知・WebView描画、GPUアップロード／GPU完了・VRAMはCLI計測へ含めない。SDKのネットワーク取得も今回の実測外で、固定revisionを事前取得したローカルパッケージを使った。新規プロセスだがOSのファイルキャッシュは消去せず、ストレージのcold-cache性能ではない。

## 実行方法

既存のPython／OpenUSDランタイムを準備し、実シーンのローカルUSDを用意する。入力は読取専用。各回のSDK作業領域・復旧の書込先は専用TemporaryDirectoryに置き、終了時に削除する。prepareの出力先は新しいディレクトリだけを許可し、既存のfixtureを上書きしない。

```sh
# 新しい場所に、同じ実シーンの.u3dと復旧コピーを作る（この準備費用は計測外）
python3 scripts/benchmark-load.py --mode prepare --input /path/to/scene.usd --fixture-dir /tmp/u3d-load-fixtures
python3 scripts/benchmark-load.py --mode usd --input /path/to/scene.usd --runs 3 --output /tmp/load-usd.json
python3 scripts/benchmark-load.py --mode open --input /tmp/u3d-load-fixtures/scene.u3d --runs 3 --output /tmp/load-open.json
python3 scripts/benchmark-load.py --mode recovery --input /tmp/u3d-load-fixtures/checkpoint.json --runs 3 --output /tmp/load-recovery.json
```

macOS／Linuxのpsを使う。`--sample-ms`は5〜1000、既定50ms。ps自体の実行時間が加わるので、実間隔はraw sampleのat_usを確認する。`--timeout`は各回の上限秒数で既定900秒。中断時はこの計測の専用プロセスグループだけを停止する。入力のSHA-256とサイズを記録し、終了後に入力の変更を検出すればレポートを確定しない。USDの入力ハッシュ／サイズはエントリファイルのものであり、参照ファイル全体のハッシュではない。

## 実測データ

NVIDIA SimReady `IV_pole_a01`、固定revision `524747d9b5e1f21b5b18b56041788c01d745f1a1`、CC-BY-4.0。[取得元](https://huggingface.co/datasets/nvidia/simready-assets/resolve/524747d9b5e1f21b5b18b56041788c01d745f1a1/IV_pole_a01/com.nvidia.simready.packaging.json)。既存アダプターでBOM・各ファイルのハッシュ／サイズを検証して一度取得した。16メッシュ・458,714三角形、既存Cubeを含め17物体。.u3dと復旧コピーはこの実メッシュから生成した約52.7 MiBの入力であり、合成形状ではない。復旧コピーの時刻等の外包情報は計測用に生成した。

macOS arm64、Rust debug。各経路3独立プロセス、経路間は並行実行していない。以下のRSSはサンプリングで観測した最大値で、厳密な割当ピークではない。

| 経路 | 読込時間中央値 | 3回の時間範囲 | Rust RSS最大 | Rust＋子RSS合計最大 |
|---|---:|---:|---:|---:|
| USD | 23.60秒 | 18.01〜24.36秒 | 97.4 MiB | 188.7 MiB |
| .u3d | 9.86秒 | 9.49〜11.44秒 | 127.9 MiB | 127.9 MiB |
| 復旧 | 15.27秒 | 13.35〜15.43秒 | 187.3 MiB | 187.3 MiB |

工程の時間中央値（ms）。各工程を別に中央値化するため、その和は全体中央値と一致しない。SDK内の工程境界はRust側の50ms status監視で観測した区間で、SDK自身の厳密な内部プロファイルではない。

| 工程 | USD | .u3d | 復旧 |
|---|---:|---:|---:|
| I/O | 7.95 | 12.30 | 10.35 |
| 解析 | 1747.15 | 3161.08 | 3593.45 |
| SDK変換等 | 2870.79 | — | — |
| SDK頂点抽出 | 2097.95 | — | — |
| 三角形化 | 3397.10 | — | — |
| 検証 | 422.52 | 85.87 | 105.36 |
| 頂点索引 | 1395.91 | 868.58 | 1173.34 |
| エッジ／面索引 | 8665.75 | 5733.96 | 8110.73 |
| 配置索引 | 0.54 | 0.30 | 0.30 |
| 採用／復旧書込 | 0.92 | 0.06 | 2219.27 |

エッジ／面索引の構築が大きな割合を占める。復旧の確定工程にはチェックポイントのJSONシリアライズと原子的書込を含み、RSSの増加点も観測した。この結果を受けた[索引構築の分離計測と高速化](surface-build-performance.md)を実装済み。復旧書込の一時バッファ削減はNextへ置く。別のシーン・release・OSの一般的性能を示すものではない。

全経路で採用後の索引保持バッファは41,038,184 bytes、配置Mapペイロード容量も含む管理対象値は41,043,784 bytes。これは入力Mesh、JSONバッファ、SDK、アロケータ、GPUを含むRSSとは異なる。最初のスナップの索引取得で全物体走査・形状再生成は0件だった。

生データ: [USD](benchmarks/load-real-usd-20261105.json)、[.u3d](benchmarks/load-real-open-20261105.json)、[復旧](benchmarks/load-real-recovery-20261105.json)。各回の工程時間、再入回数、RSS時系列、工程別RSSピークとサンプル数、入力ハッシュ、索引容量、最初の問い合わせを記録した。

## メモリ値の解釈

RSSはRustプロセス単体と、その時点で存在する子孫プロセスのRSSの和を分ける。無関係なプロセスは含めない。SDK子プロセスのRSSはGPUメモリではなく、RSSの和は共有ページを重複計上し得る。psの負荷・スケジューリングとstdoutの時計アンカーの遅延もある。今回の実間隔の中央値は約83〜114msで、設定50msより長かった。

短いI/O・配置索引・採用ではRSSサンプルがない場合がある。`samples: 0`と`null`は未観測であり、メモリ使用ゼロを意味しない。観測できなかった工程の時間も0と補完せず、summaryのphase_observed_runsを残す。初回実測のrawに残る終了済みプロセスのRSS 0の行は、exit_zero_rss_samples_excludedに計上し工程別メモリ集計から除外した。更新後のサンプラーはその行を採用しない。

## アプリ内の時間診断API

`get_load_profile`は、実行中または直前の一操作の小さな集計を返す。Open／Recovery／USD／Snapの種別、ID、outcome（running／success／cancelled／failed）、commit_started、total_us、phases（phase、elapsed_us、entries）を持つ。同じ工程へ複数回入る場合は時間を合算し、entriesに回数を記録する。履歴は一件だけ保持し、ファイル名・パス・形状を送らない。通常の進捗ポーリングのデータ量は変更しない。

```ts
const profile = await invoke("get_load_profile");
```

成功は通知／CPU描画送出が成功した後に確定する。cancelled／failedも所有GuardのDropで記録し、古いIDからの成功通知は拒否する。commit_startedは確定境界へ入ったことを表し、採用や書込の成功を保証するフラグではない。failedかつcommit_startedの場合には、既に文書が変更されている可能性がある。

アプリではnotify_drawにTauri通知送信とCPU側描画コマンド送出も計上するが、GPU完了やWebViewイベント処理／paintは待たない。描画が無効なら通知だけとなる。この区間は上記ヘッドレス実測にはない。初期Readにはワーカーへのディスパッチ待ち等も含み、各値は関数単体のCPU命令時間ではなく工程区間の経過時間である。通常アプリではstdoutのtraceを出さず、明示的な計測テストだけがstageイベントを出す。

RSSサンプリングは明示実行のCLIだけで行う。通常アプリのUIスレッドに定期的なps実行を追加していない。

⭕️ [Pending] ネイティブUIを含む実プロセス全体のRSS・GPU完了／VRAM、release比較、LinuxでのRSS実測。実ユーザー文書・起動中のアプリは操作していない。
