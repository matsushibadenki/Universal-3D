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
| 更新 | 操作時に再描画 | dirty object命令、描画インスタンスは再構築 | ジオメトリ共有・GPU行列変換を先行し、差分GPU更新は別工程 |

ThreeXの64-byte little-endianコマンド形式はエンジン境界の参考とする。
現行のTauri IPCや.u3d形式を、そのバイナリ形式に直接置き換えない。
ThreeXの階層検証とバッチの原子的検証は、Universal-3Dの階層編集を追加する際に参考にする。

## 進め方

- 🟢 [Done] 共通技術、所有境界、ID・変換契約の差異を確認し、参照リンクをGit除外。
- 🟠 [Next] GPU行列変換とジオメトリ共有の実装時に、ThreeXのインスタンス形式と変換結果を比較。
- 🟠 [Next] 複合回転、負スケール、ミラー、削除／Undoで互換性を確認してから共有crateの境界を決定。
- 🔴 [Later] 安定したGPUコアをバージョン付きcrateとして共有し、TauriとWASMの接続層を分離。
- ⭕️ [Pending] ThreeXのWASMビルド・ブラウザGPU描画は参照先READMEで未検証。本作業では追加検証していない。

共通化の完了条件は、参照リンクを取り除いてもUniversal-3D単独でビルド・テスト・起動できること。
