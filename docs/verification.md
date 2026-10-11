# Verification — 2026-10-11

## Visual reference and evidence

- Built-in ImageGenによる画面設計: `concept.png`。最終アプリに画像を貼り付ける使い方はしていません。
- 最終UI: `ui-desktop.png`。Codex IABで設計画像と同じ1586×992pxで取得。
- GPU描画: `native-viewport.png`。macOSの実アプリで取得。
- 設計画像と実装画像を両方 `view_image` で目視比較しました。

生成時の仕様: Universal 3Dのプロ向け暗色3D編集画面。Scene、Viewport、Inspector、Timeline、追加／Undo／Redo／Save、言語選択。Graphite背景、lime選択色、細い区切り、読みやすい小さなUI文字、カードやマーケティング表現は使用しない。

## Fidelity ledger

| 検査点 | 設計の意図と最終実装 | 差分／対応 |
| --- | --- | --- |
| 構成 | 左Scene、中央Viewport、右Inspector、下Timeline | 同じパネル階層。ネイティブ描画面は別ウインドウという初期版の差分を明示 |
| 配色 | Graphite背景、薄いlimeアクセント、細い境界 | CSSトークンに統一。GPU面はsRGBの明るさを確認して背景とグリッドを補正 |
| 文字 | 見出し／属性／ボタンの階層と読みやすさ | 各コントロールのフォントを指定。英語、日本語、简体中文に翻訳 |
| 余白 | パネル境界、控えめな角丸、左右余白 | 左右16px。狭い幅はパネルを縦配置 |
| 操作 | 追加、選択、Transform、Undo/Redo、パネル分離 | デスクトップでは実コマンド。ブラウザでは無効化しプレビューを明示 |
| アセット | ネイティブ3Dシーンを操作可能な表示にする | 静止画の3Dフェイクを使用せず、Rust/wgpuの実描画 |
| 表示文言 | 実装済み機能を示す | 未実装のAnimationタブ、メニュー、再生・Material操作は設計画像から省略。Open、独立Viewportの説明、未実装Timeline評価の説明を追加 |

原案のすべての機能・レイアウトを再現したという扱いではありません。意図的な差分は独立Viewport、固定ドック位置、未実装機能の省略です。今回のMVPの範囲内で、配色・階層・余白・コントロールを確認しました。

## Functional checks

- `npm run build`: TypeScriptとViteのビルド成功。
- `cargo test --manifest-path src-tauri/Cargo.toml --offline`: 3件成功。履歴復元、Undo後のRedo破棄、不正Importの拒否。
- Tauri debug `.app`のパッケージ作成成功。
- 実アプリで立方体のGPU表示、球体追加、X位置2、Undoで0／Redoで2を確認。
- Inspectorを分離し、同じ選択とX位置2を確認。分離パネルでMirrorを有効にし、再ドッキング後もMirrorが有効。
- `.u3d`保存と再読込で位置・Mirrorを復元。サンプル文書は `demo.u3d`。
- 共有フレームを分離Timelineに引き継ぐことを確認。
- フレーム番号を打鍵毎に確定して2桁入力が途切れる問題を修正。修正版で42の入力・確定を確認。
- 未保存の終了確認はメインウインドウとCmd+Q両方を対象に実装。最終版でCmd+Q→確認シート→Noを操作し、シーンを保持できることを確認。macOSのQuitを独自メニュー命令に置き換え、確認シートはメインウインドウに紐付けています。
- IABで3言語×320/375/414/768pxを検査。scrollWidthは各viewport幅と一致し、button/h1/h2の右端はviewport内。

## Intentional limits

アニメーション評価、任意位置のドラッグドッキング、3D選択／ギズモ、ネイティブ描画面の中央埋め込み、OpenUSD/glTF、配布用署名・公証は未実装です。Windows/LinuxはこのmacOS環境では実機検証していません。
