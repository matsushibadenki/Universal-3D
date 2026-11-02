# ThreeXとの協調開発

2026-10-11に参照先のREADME、Cargo workspace、Rust coreを確認。
ThreeXはThree.js互換APIを目指すRust/WASM/wgpuエンジン、Universal-3DはTauriの制作アプリとして開発する。

## 独立性

- 参照用シンボリックリンクは一時的なもの。依存定義、ビルド、実行、保存データにそのパスを使用しない。
- Universal-3DのDocument、Undo/Redo、選択、パネル状態は引き続きRust側で所有する。
- 共通エンジンを導入する場合は、独立したバージョン付きcrateまたは固定リビジョンの依存として扱う。参照先へのpath依存は追加しない。
- ThreeX側のファイルはこの確認では変更していない。共有コードの抽出は両プロジェクトの契約を確認してから行う。

## 共通方針と差異

| 項目 | Universal-3D | ThreeX | 協調方針 |
| --- | --- | --- | --- |
| 描画 | ネイティブwgpu 27 | wgpu 27、WASM向けAPI | GPUコアとウインドウ／Canvas接続を分離する |
| データ所有 | Rust Document/Editor/Renderer | Rust Scene/GPU buffers | JavaScriptは命令・状態のみ扱う |
| オブジェクトID | u64、JavaScript安全整数の範囲 | u32、0予約 | アダプタで対応表を持ち、切り捨て変換しない |
| 変換 | 制作UIと現行rendererの契約 | XYZ Euler radians、列形式の行列 | 単位と合成順序をテストで確認してから接続する |
| 形状 | cube/sphere/plane、ミラー | 共有BoxGeometryとインスタンス描画 | 共通化しても既存形状とミラーを維持する |
| 更新 | 前回値との比較で連続変更範囲のみGPU転送 | scene stampと変更範囲によるGPU転送 | 範囲転送と再確保時の全量初期化を参考に独立実装 |

ThreeXの64-byte little-endianコマンド形式はエンジン境界の参考とする。
現行のTauri IPCや.u3d形式を、そのバイナリ形式に直接置き換えない。
ThreeXの階層検証とバッチの原子的検証は、Universal-3Dの階層編集を追加する際に参考にする。

## 進め方

- 🟢 [Done] 共通技術、所有境界、ID・変換契約の差異を確認し、参照リンクをGit除外。
- 🟢 [Done] ThreeXの形状共有方針を参考に、Universal-3D単独の形状キャッシュとGPUバッファ・深度テクスチャ再利用を実装（2026-10-12）。
- 🟢 [Done] ThreeXのモデル行列＋色のインスタンス方式を参考に独立実装。複合回転の契約差があるため、既存の度指定・X→Y→Z適用順を維持（2026-10-12）。
- 🟢 [Done] 既存変換との比較、負スケール、ワールドミラー、カメラ投影、表示バッチとUndo/Redoをテスト。
- 🟠 [Next] 共有crateの境界を決定する前に、ThreeXとの変換契約と多形状対応を調整。
- 🔴 [Later] 安定したGPUコアをバージョン付きcrateとして共有し、TauriとWASMの接続層を分離。
- ⭕️ [Pending] ThreeXのWASMビルド・ブラウザGPU描画は参照先READMEで未検証。本作業では追加検証していない。

共通化の完了条件は、参照リンクを取り除いてもUniversal-3D単独でビルド・テスト・起動できること。

## 差分更新の協調確認

2026-10-12にThreeX rendererのscene stamp／instance_updates_sinceと範囲転送を再確認。
Universal-3Dは自分の形状別バッファに前回値を保持し、連続した変更範囲を転送する。
ThreeXのstampやcoreを依存として導入せず、既存Document・Undo/Redoから生成される表示値を比較する。
2026-10-14にTransform比較キャッシュを追加し、未変更行列の再生成を省略。全オブジェクトの走査とインスタンス生成・比較は残っているため、revision／変更IDによる走査削減は次工程。

描画側は独自の128件の変更ID履歴とrevisionを参照する方式へ更新。通常の単体編集・選択では対象スロットだけを更新し、構造変更は対象形状バッチ内で順序を維持して局所更新し、履歴欠落・文書置換で全体再構築する。ThreeXのコードや参照リンクへの依存はない。


## 頂点スナップ索引の協調確認（2026-10-24）

ThreeXの`gather-cache.md`、`partitioned-gather.md`と、coreの`instance_updates.rs`、`assets.rs`を読み取り専用で確認した。参考にしたのは、共有リソースと配置情報を分けること、revision＋変更履歴で再利用可否を決めること、履歴欠落／異なる文書では再構築すること、アセット名だけを同一性の根拠にしないことである。

Universal-3Dでは既存のrender_revision・128件の変更ID履歴・文書epochと、所有しているimmutable MeshのArc同一性を利用する。形状の頂点BVHは共有し、配置の動的境界木は変更対象だけ修復する。カメラと画面サイズは索引の内容に含めず、問い合わせ時のコーンへ反映する。移動元の選択物はIDで直接取得し、吸着先の除外IDは問い合わせ側で保持する。

Morton／radixの頂点索引、高さを保つ動的境界木、ローカル空間へのコーン変換とそのテストはUniversal-3D側で独自に実装した。ThreeXのソースを流用せず、参照先の変更、path依存、実行時リンク、保存形式・IPCの変更も追加していない。


## エッジ／面の事前構築と容量計測（2026-10-30）

ThreeXの`docs/mesh-budget.md`と`docs/gather-cache.md`を読み取り専用で確認した。共有形状と配置の容量を別に数えること、論理バッファ容量とプロセス／GPU実使用量を区別すること、一時領域の生存期間を短くすることを参考にした。コードは流用せず、ThreeXへのpath依存・実行時リンク・保存参照は追加していない。

Universal-3Dの既存読込ワーカーでSurfaceIndexも準備し、同一immutable Meshの既存GeometryにはSurfaceIndexのArcだけを採用する。頂点順序は同一形状で決定的であり、既存スナップショットの頂点配列とシーン索引を保持できる。エッジ境界配列は面境界配列の構築前に解放する。容量は索引所有Vec／Boxの容量、マップの論理ペイロード、ソースメッシュ容量、実測RSSを区別する。詳細と再計測手順は[snap-index-performance.md](snap-index-performance.md)。前節の「事前構築はNext」は今回完了。


## 探索・初回配置木の短縮（2026-11-02）

Universal-3D内のSurfaceIndexに、近傍優先の訪問順と現在候補に応じたコーン縮小を追加した。BoundsTreeの一括構築では、葉Mapとparent／heightを同時に初期化し、既存の局所修復をそのまま使う。追加の共有ライブラリ・ThreeX参照・コード流用はない。頂点・エッジ・面のデータはRustが保持し、UIへの大量転送は行わない。前回の性能計測から優先した変更であり、[再計測結果](snap-index-performance.md)を保存した。


## 文書と配置木の一括採用（2026-11-02、後続工程）

.u3d読込・復旧ではUniversal-3D独自のPreparedDocumentを使い、文書そのものと形状／配置索引を同じワーカーで準備する。別文書へ完成済みの配置木だけを採用できない所有構造とし、共有形状のArc・epoch／revision・既存の取引Stampを使用する。描画やIPCへ配置の大きな配列を追加せず、変更後は既存の局所修復に戻る。ThreeXへの追加依存やコード流用はない。メモリ予算と進捗は次工程として残す。


## 新規索引の構築予算（2026-11-02、後続工程）

共有形状と配置を分けて数える方針を維持し、Universal-3D独自の構築用予約値と予算検査を追加した。Meshの不変Arcは一回だけ数え、メッシュの既存保持データ・プロセスRSS・VRAMとは区別する。ThreeXのコードやモジュールの追加流用はなく、path依存・実行時参照・リンクも追加していない。予算の対象と限界は[snap-index-budget.md](snap-index-budget.md)。
