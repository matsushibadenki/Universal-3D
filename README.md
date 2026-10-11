# Universal 3D

ThreeXとの協調方針とエンジン境界は [docs/engine-coordination.md](docs/engine-coordination.md) に記録しています。参照用シンボリックリンクへのビルド・実行依存は追加しません。

Tauri 2 + Rust + wgpuによる統合3D制作アプリの初期実装です。現段階は基本シーン編集ができるMVPであり、Maya・3ds Max相当の全機能はまだ備えていません。

## 起動

```sh
npm install
npm run tauri dev
```

macOS検証用アプリ: `src-tauri/target/debug/bundle/macos/Universal 3D.app`。ビューポートを開くボタンでRustのGPU描画ウインドウを起動します。

```sh
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri build -- --debug --bundles app
```

`npm run dev` はブラウザでUIを確認するためのプレビューです。ブラウザでは編集操作を無効化しています。

## 使い方

1. 立方体・球体・平面を追加し、Sceneで選択します。
2. Inspectorで位置・回転（度）・スケール・名前・表示を変更します。数値はEnterまたはフォーカス移動で確定します。
3. ワールドX軸ミラーは原形状を保ったまま複製を描画します。
4. カメラの水平回転・仰角・距離でビューを変更します。スライダー操作終了時に描画します。
5. パネル見出しの分離ボタンでフローティング、分離パネルのドッキングボタンで元の位置に戻します。
6. 保存／開くは `.u3d` JSON形式。Cmd/Ctrl+Z、Shift+Cmd/Ctrl+Z、Cmd/Ctrl+Sを利用できます。

## データ所有と描画

- `src-tauri/src/document.rs`: RustがDocument、選択、履歴、カメラ、現在フレーム、パネル配置を所有。
- `src-tauri/src/main.rs`: コマンド実行、全ウインドウへの状態通知、保存／読込、ウインドウ管理。
- `src-tauri/src/renderer.rs`: Rustによるプリミティブ生成・変換、wgpuによる深度付きGPU描画。
- `src-tauri/src/scene.wgsl`: GPUシェーダー。
- `src/`: ReactのUI。ウインドウはDocumentを所有せず、共有状態のスナップショットを表示し、命令を送ります。入力途中の文字列など表示のための一時状態のみ保持します。

IPCは命令とオブジェクトの属性のみ。頂点・ピクセル・フレームデータはJavaScriptへ転送しません。初期版は操作時に再描画する構成です。大規模シーンに向けたGPUバッファ再利用、GPUでの行列変換、差分更新は今後の工程です。

## 検証と制限

macOSでwgpu描画、追加、Transform変更、Undo/Redo、Inspector分離と再ドッキング、分離パネルからのミラー変更、保存／再読込を確認しました。Rustテスト3件が成功。UIは英語・日本語・简体中文の320/375/414/768px幅で横のはみ出しを検査しています。

ビューポートは独立したネイティブウインドウです。中央パネルへの埋め込み、ドラッグで任意位置へのドッキング、ギズモ、マウスによる3D選択は未実装です。Timelineは共有フレーム指定のみで、キーフレーム・再生・アニメーション評価は未実装です。現在の固定プリミティブに対するミラーは、汎用Modifier Stackの最初の足場です。

未保存の文書を開く時とメインウインドウを閉じる時とCmd+Qで終了する時には破棄確認を表示します。OSの強制終了・プロセス停止に備える自動保存は未実装です。検証用ビルドはデバッグ版で、配布用の署名・公証は行っていません。

設計図と確認結果: [docs/verification.md](docs/verification.md)。進捗: [ROADMAP.md](ROADMAP.md)。
