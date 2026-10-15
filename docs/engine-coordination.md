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
