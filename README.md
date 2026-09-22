# KSIP

Windows用のSIPクライアントです。Tauriの画面、Rustの状態管理、baresipのSIP/RTP処理、Google WebRTCのWindows音声デバイス層とAudio Processingを単独exeへ静的リンクします。

SIPサーバに自動REGISTERし、保留、アテンド転送、コールパーク、PAIによる番号更新、通話履歴、自動録音、音声デバイス選択、などが可能です。画面上部には登録に使われた接続方式（UDPかTLS）を、下部には通話中のコーデックとRTPの暗号化方式を表示します。いずれも設定値ではなく交渉の結果で、通話していないときは空欄です。通話履歴の番号は、右のボタンでクリップボードへコピーできます。

信号はUDPのほかにTLSを選べます。TLSのときは音声もSRTPで暗号化でき、鍵の運び方はSDESとDTLSのどちらかを選びます。接続先の証明書は、信頼する認証局の証明書を指定したときだけ検証します。

音声コーデックはOpus、G.722、G.711（μ-law・A-law）を、この順で提示します。相手が持っているものの中で最初に一致したものが選ばれるため、対応していないサーバーや電話機とはG.711で通話します。OpusはVoIP用途向けに、モノラル・32 kbps・帯域内FEC有効で使います。無線LANのパケットロスを前提にした設定です。

フロントエンドのnpm依存はありません。音声はbaresip内で処理し、RustやWebViewを経由しません。通常起動と同じexeを `--engine` で子プロセスとして起動します。

## 設定の保存場所と秘密情報

運用に必要なものは、すべて `ksip.exe` と同じフォルダに置きます。フォルダごとコピーすれば移動できます。

- 一般設定：`HKCU\Software\KashiharaCity\ksip`

パスワード以外の設定はこちらです。下の表の項目は、このキーの `Settings` 値（REG_SZ）にJSONオブジェクト1つとして入ります。

**キーが無い場合と、空の値を保存した場合は同じ扱いです。** どちらも「値が無いとき」の欄のとおりに動くため、初回起動時にキーが無くても動きます。

接続先と駐車番号だけは、このキー直下の独立したレジストリ値（REG_SZ）です。ADのグループポリシーで一括配布できるようにするためで、`Settings` のJSONには含みません。

| キー                   | 型     | 意味                                                                                                                         | 値が無いとき                                          |
| ---------------------- | ------ | ---------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------- |
| `network_adapter`      | 文字列 | 使うネットワークアダプター。`GetAdaptersAddresses()` の `AdapterName`（`{GUID}` 形式）                                       | 全インターフェイスで待ち受け、送信元はWindowsが選ぶ   |
| `sip_port`             | 整数   | ローカルSIP待受ポート。1024以上                                                                                              | `5060`                                                |
| `rtp_port`             | 整数   | RTP開始ポート。1024〜65400の偶数で、そこから20ポートを使う。SIPポートと重ならないこと                                        | `10000`                                               |
| `microphone`           | 文字列 | 録音エンドポイントID。500文字以内                                                                                            | `default`（Windowsの既定の通信マイク）                |
| `speaker`              | 文字列 | 再生エンドポイントID                                                                                                         | `default`（Windowsの既定の通信スピーカー）            |
| `microphone_gain`      | 整数   | マイクのソフト増幅率（%）。100〜200                                                                                          | `100`（増幅しない）                                   |
| `speaker_gain`         | 整数   | スピーカーのソフト増幅率（%）。100〜200                                                                                      | `100`（増幅しない）                                   |
| `auto_record`          | 真偽   | 通話成立時に自動録音するか                                                                                                   | `false`（録音しない）                                 |
| `auto_answer`          | 真偽   | 着信を即座に応答するか。動作確認用                                                                                           | `false`（自動応答しない）                             |
| `aec`                  | 真偽   | エコーキャンセルを有効にするか                                                                                               | `true`（有効）                                        |
| `aec_delay_ms`         | 整数   | AECの遅延ヒント（ミリ秒）。0〜500。ADMが実測値を返すときはそちらを優先                                                       | `20`                                                  |
| `register_interval`    | 整数   | REGISTERの更新周期（秒）。30〜3600                                                                                           | `300`                                                 |
| `detail_log`           | 真偽   | 障害調査用の詳細をログへ出すか。SIPメッセージ全文（認証ヘッダーは伏字）、baresipのデバッグ行、WebRTC音声処理の情報行が増える | `false`（出さない）                                   |
| `browser_dial_confirm` | 真偽   | リンクからの発信前に確認するか                                                                                               | `true`（確認する）                                    |
| `shortcut_window`      | 文字列 | ウィンドウを出す・しまうグローバルショートカット。`SHIFT+F2` のように修飾キーとキーを `+` で区切る                           | ショートカットを使わない                              |
| `shortcut_call`        | 文字列 | 電話に出る・切るグローバルショートカット。書き方は同じ                                                                       | ショートカットを使わない                              |
| `incoming_action`      | 文字列 | タスクトレイにいるときの着信の扱い。`show`＝ウィンドウを出す、`notify`＝タスクバーへ通知する                                 | `show`（ウィンドウを出す）                            |
| `language`             | 文字列 | 画面の言語。`ja`・`en`・`zh-TW`                                                                                              | 空欄。Windowsの表示言語に合わせ、対応が無ければ日本語 |
| `sound_ring`           | 文字列 | 着信音に使うWAVのパス。400文字以内                                                                                           | 内蔵音を鳴らす                                        |
| `sound_ringback`       | 文字列 | 呼出音に使うWAVのパス                                                                                                        | 内蔵音を鳴らす                                        |
| `sound_callwaiting`    | 文字列 | 通話中の着信に使うWAVのパス                                                                                                  | 内蔵音を鳴らす                                        |
| `sound_busy`           | 文字列 | 話中に使うWAVのパス                                                                                                          | 内蔵音を鳴らす                                        |
| `sound_notfound`       | 文字列 | 宛先なしに使うWAVのパス                                                                                                      | 内蔵音を鳴らす                                        |
| `sound_error`          | 文字列 | エラーに使うWAVのパス                                                                                                        | 内蔵音を鳴らす                                        |

`network_adapter` を指定すると、登録もRTPもそのアダプターから出ます。待ち受けるアドレスは、保存した値ではなく**接続のたびにアダプターから解決**します。アドレスが変わったときは、通話中でなければ自動で接続し直します（通話中は終わるまで待ちます）。指定したアダプターが無い場合は「指定NICが見つかりませんでした」、アドレスを持たない場合は「指定NICのIPアドレスが取得できませんでした」として保存・接続を拒否します。

`sound_ring`〜`sound_error` に指定したファイルは、起動時に16bit PCMへ変換します。読めない形式や見つからない場合も内蔵音のままとし、理由をログへ残します。

範囲外の値や制御文字を含む文字列は保存時に拒否します。

値ごとのレジストリ値（いずれもREG_SZ）は次の6つです。設定画面から保存したときも、この形で書き戻します。

| 値名                  | 意味                                                                                                                                                 | 値が無いとき               |
| --------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------- |
| `server`              | SIPサーバーのホスト名かIPアドレス。半角英数字と `.` `-` のみ、253文字以内                                                                            | `127.0.0.1`                |
| `port`                | SIPサーバーのポート。1〜65535                                                                                                                        | `5060`                     |
| `extension`           | 内線番号。半角英数字と `_` `.` `+` `-` のみ、100文字以内                                                                                             | `auth_user` と同じ値を使う |
| `park_slot_1`         | 保留1の駐車番号。1〜20桁の数字で、互いに重複しないこと                                                                                               | 保留1のボタンを使わない    |
| `park_slot_2`         | 保留2の駐車番号                                                                                                                                      | 保留2のボタンを使わない    |
| `park_slot_3`         | 保留3の駐車番号                                                                                                                                      | 保留3のボタンを使わない    |
| `transport`           | SIPの信号に使う方式。`udp` か `tls`。設定画面で切り替えると、ポートが既定値のときだけ5060と5061を入れ替えます                                        | `udp`（暗号化しない）      |
| `media_encryption`    | 音声の暗号化。`sdes`＝SRTP（鍵を信号に載せる）、`dtls`＝SRTP（鍵をメディア経路で交換）                                                               | 暗号化しない               |
| `ca_file`             | 信頼する認証局の証明書（PEM）のパス。400文字以内                                                                                                     | 接続先の証明書を検証しない |
| `browser_integration` | `ksip:` のリンクを受け付けるか。`true`・`1`・`yes`・`on` でON。ONのとき、起動時と設定保存時に `HKCU\Software\Classes\ksip` へ登録し、OFFにすると消す | 受け付けない               |

`ca_file` を指定しない場合、通信は暗号化されますが接続先が本物かどうかは確かめないため、中間者攻撃を防げません。`media_encryption` に `sdes` を選ぶ場合は `transport` を `tls` にしてください。SDESは鍵を信号に載せるため、信号が平文では意味がありません（保存時に拒否します）。

同じ組織の他のアプリと並ぶ配置です。

```text
HKCU
└─ Software
   └─ KashiharaCity
      ├─ CwfChecker
      └─ ksip
```

テスト用の使い捨てプロファイルは `KashiharaCity\ksip\Test\<プロファイル名>` に作られ、終了時に削除します。

- SIPアカウント：Windows資格情報マネージャー（汎用資格情報 `KSIP/SIP/default`）

秘密にすべき2項目だけがこちらに入ります。JSONではなく、資格情報そのものの欄を使います。

| 欄         | 意味                                                                  |
| ---------- | --------------------------------------------------------------------- |
| ユーザー名 | 認証ID（`auth_user`）。半角英数字と `_` `.` `+` `-` のみ、100文字以内 |
| パスワード | SIPパスワード。512文字以内、制御文字は不可                            |

この2つには既定値がありません。両方そろって初めてアカウントが設定済みになります。

この資格情報が無いうちはアカウント未設定として扱い、接続を試みずに起動時に設定画面を開きます。パスワードは画面へ返しません。認証IDが同じであれば、パスワード欄は空のままで保存できます。サーバーやポート、内線番号を変えるだけならパスワードは要りません。資格情報に入っているのは認証IDとパスワードだけで、接続先はレジストリにあるためです。認証IDを変更する場合はパスワードの再入力が必要です。

- 通話履歴：exeと同じ場所の `call-history.json`（画面の「クリア」で消去できます）
- ログ：exeと同じ場所の `ksip-log.json`（直近1000行。画面の「クリア」で消去できます）
- 録音：exeと同じ場所の `recordings/`（最初に録音したときに作ります）

エンジンへ渡す設定は、保存した内容から毎回作り直すため `%TEMP%\ksip-profile\` に置きます。着信音・呼出音など6つの音も同じ理由でexeに埋め込んであり、起動時に `%TEMP%\ksip-sound\` へ展開して使います。設定画面で1つずつ別のファイルを指定でき、指定が無いときと指定先が見つからないときは埋め込みの音に戻ります。

### 電話を受けたくないとき

会議中など、KSIPを終了せずに着信だけ止めたいときは、右上の「接続解除」を押します。SIPサーバーへの登録を解除するので、以後この端末へは呼び出しが来ません。状態表示は「接続解除済み — 着信しません」になり、こちらからの発信もできません。

戻すときは「再接続」を押します。登録し直して、着信も発信もできるようになります。KSIPを起動し直したときも登録済みの状態で始まります。

ヘッダーは左のランプと2行で状態を示します。1行目が内線番号と状態、2行目が接続先（サーバー:ポートと通信方式）です。2行目は**登録できているときだけ**出ます。ランプは緑（登録済み）・琥珀（接続解除・接続中）・赤（登録失敗）です。

### 転送

「転送」は通話1を通話2へつなぎます。SIPサーバーによっては、この向きのREFERを断るものがあります。断られた場合はKSIPが自動で逆向き（通話2から通話1へ）を一度試し、それも駄目なときだけ失敗として知らせます。利用者の操作は変わりません。

### 通話が2本あるとき

2本目に発信すると、1本目は自動的に保留になります。2本目を切ると1本目へ戻り、画面に「通話を終了し、保留中の通話に戻りました。」と出ます。保留のまま置いておきたい場合は、戻ったあとに保留ボタンを押してください。

転送の結果も同じ場所に出ます。エンジンは結果の名前（`TRANSFER_DONE` など）だけを返し、画面がその語を選びます。

### ログの読み方

KSIPは2つのプロセスで動きます。画面を持つ本体と、同じexeを `--engine` 付きで起動した音声エンジン（baresip）です。ログはこの両方から集めて1本にまとめたもので、画面のログタブと `ksip-log.json` に同じものが出ます。

画面では各行が次の形で出ます。

```text
9/21 9:12:54 [engine] REGISTER_OK 200 OK
```

先頭はローカル日時、続く `[ ]` はその行がどこから来たかを表します。4種類あります。

| タグ     | 由来                                                             | 主な中身                                                                                                                                                 |
| -------- | ---------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `engine` | 音声エンジンの標準出力・標準エラーを本体が1行ずつ取り込んだもの  | baresip本体のメッセージ、SIPメッセージ全文（`ksip sip >` `ksip sip <`）、音声デバイス（`wasapi/…`）、WebRTCの音声処理（`(…cc:行):`）、録音（`postlab:`） |
| `event`  | 音声エンジンが制御チャネルで本体へ送る通知を、本体が整形したもの | `REGISTERING`、`REGISTER_OK`、`CALL_ESTABLISHED`、`CALL_CLOSED` など大文字で始まる行                                                                     |
| `app`    | 本体そのもの                                                     | 画面に出したエラーと同じ文言、音声エンジンの異常終了や制御接続の切断、パニック                                                                           |
| `ui`     | 画面（WebView）                                                  | 操作や定期取得の失敗、スクリプトエラー。同じ文言は1分に1回までにまとめます                                                                               |

補足が3つあります。

- 画面にエラーを出したときは、同じ文言が必ずログにも残ります。後から見れば、利用者が何を見たのかが分かります。
- `engine` の行に付く時刻は、エンジンが出した時刻ではなく本体が受け取った時刻です。ミリ秒単位のずれがあります。
- `engine` の量は `detail_log` で変わります。ONにするとSIPメッセージ全文、baresipの詳細、WebRTC音声処理の情報が加わります。OFFのときでも、警告とエラーは出ます。

`ksip-log.json` には、画面の見た目ではなく行の中身がそのまま入ります。時刻はタイムゾーンつきのRFC 3339なので、別の端末へ送っても、年をまたいでも、いつの記録かが確定します。画面に出す形（`9/21 9:12:54`）はこのファイルから組み立てています。

エンジン（baresip）の行は、そのエンジンが書いた文がそのまま `text` に入ります。本体（`app`・`ui`）の行は、**文ではなく起きたことの名前**を `code` に、文に差し込む値を `args` に持ちます。

```json
[
  {
    "time": "2026-09-21T09:12:54+09:00",
    "src": "engine",
    "text": "REGISTER_OK 200 OK"
  },
  {
    "time": "2026-09-21T09:13:02+09:00",
    "src": "app",
    "code": "ENGINE_EXITED",
    "args": ["exit code: 3"]
  }
]
```

### 言語

画面は日本語・英語・繁体字中国語で出せます。設定の「表示する言語」で切り替えると、その場で切り替わります。既定は空欄で、Windowsの表示言語に従い、対応する言語が無ければ日本語になります。`ja-JP` のような地域つきの指定は `ja` として扱い、`zh-HK` や `zh-Hant` は `zh-TW` として扱います。

英語と繁体字中国語の文言は開発者による初訳で、母語話者の校閲を経ていません。各ファイルの先頭にもその旨を書いてあります。

### 文言はどこにあるか

本体もエンジンのKSIPモジュールも、文を持ちません。返すのは `ENGINE_EXITED` のような**名前**と、文に差し込む値だけです。画面のHTMLも同じで、文の代わりに `data-i18n="SETTINGS_SERVER"` のような名前だけを持ちます。

文そのものは `src-web/locales/` に言語ごとのファイルとして置いてあります。

```text
src-web/locales/ja.js      日本語
src-web/locales/en.js      英語
src-web/locales/zh-TW.js   繁体字中国語
```

選んだ言語に訳が無い名前は日本語で出し、日本語にも無ければ名前のまま出します。画面のどこかが空になることはありません。

Windowsが自分で描くもの（タスクトレイのメニュー、ファイル選択ダイアログ、通知）だけは画面の表に届かないので、本体側の小さな表（`src-tauri/src/message.rs`）が文言を持ちます。

`scripts/test/i18n.py` が、報告される名前と表の文言が食い違っていないかを検査します。

## ショートカットキーと着信の知らせ方

`shortcut_window` と `shortcut_call` は、ほかのアプリを使っている間も効くグローバルショートカットです。どちらも既定は空欄で、空欄のキーは登録しません。

| 設定              | 押したとき                                                                 |
| ----------------- | -------------------------------------------------------------------------- |
| `shortcut_window` | ウィンドウが出ていればタスクトレイへしまい、しまってあれば出して前面にする |
| `shortcut_call`   | 着信中なら応答し、それ以外なら選んでいる通話を切る                         |

書き方は `SHIFT+F2`、`CONTROL+ALT+K`、`F3` のように、修飾キーとキーを `+` で区切ります。書き方が違うときは「修飾キーとキーは「+」で区切ってください（例: SHIFT+F2）」、ほかのアプリが既に使っているキーのときは「ほかのアプリで使用されていないキーを指定してください」と設定画面へ出して、保存しません。保存を断ったときは、それまで効いていたキーがそのまま残ります。2つの設定に同じキーは指定できません。

`incoming_action` は、**タスクトレイにいるとき**の着信の扱いです。`show` なら今までどおりウィンドウが出てきます。`notify` ならウィンドウはしまったままで、「〇〇から着信」とWindowsの通知を出します。電話に出ればウィンドウが出てきます。ウィンドウが既に出ているときは、どちらの設定でも変わりません。

### 通知に必要なレジストリ

Windowsの通知は、アプリの識別子（AUMID）をたどって表示名とアイコンを決めます。KSIPは起動のたびに、現在のユーザーへ次を書きます。管理者権限は要りません。

```text
HKCU\Software\Classes\AppUserModelId\local.ksip.client
  DisplayName = KSIP
  IconUri     = <ksip.exeと同じフォルダ>\ksip-notification.ico
```

アイコンは起動時に実行ファイルの横へ書き出します。書けないフォルダに置いた場合は、アイコンなしで通知を出します。

**この方法について。** Microsoftの公開文書は、AUMIDの割り当て方としてスタートメニューのショートカットと `SetCurrentProcessExplicitAppUserModelID` を説明していますが、ここで使っている `HKCU\Software\Classes\AppUserModelId\<識別子>` への登録だけで通知が出ることは文書化されていません。実機で確認した挙動に基づく実装です。Windowsの更新で通知が出なくなった場合は、スタートメニューへショートカットを置く方法へ切り替えてください。

## ブラウザ連携

`browser_integration` をONにすると、WebページやメールのリンクからKSIPを操作できます。リンクは次の5つです。

| リンク            | 動作                                                |
| ----------------- | --------------------------------------------------- |
| `ksip:<番号>`     | その番号へ発信。`-`、空白、括弧、`.` は取り除きます |
| `ksip:ANSWER`     | 着信に応答                                          |
| `ksip:HANGUP`     | 選択中の通話を切断                                  |
| `ksip:SHOWWINDOW` | ウィンドウを前面に出す                              |
| `ksip:APP_QUIT`   | KSIPを終了                                          |

KSIPが起動していないときは、リンクがKSIPを起動し、SIPサーバーへの登録が終わってから実行します（最大25秒）。`APP_QUIT` は起動していなければ何もしません。リンクで届いた指示は、すべてログへ `[app] protocol …` として残ります。

**発信の確認について。** Webページは利用者の操作なしにリンクを開けることがあります。`browser_dial_confirm` を既定の `true` のままにしておくと、発信前に「<番号> に発信しますか」と尋ねます。これをOFFにすると、開いているページが無断で発信できるようになります。タスクトレイにしまってあるときは、尋ねる前にウィンドウを出して前面にします。確認をOFFにしている場合は、しまったまま発信します。

### レジストリ

`browser_integration` がONのとき、KSIPは起動時と設定保存時に次を書きます。現在のユーザーにだけ登録するので管理者権限は要りません。OFFにすると `ksip` キーごと削除します。

```text
HKCU\Software\Classes\ksip
  (既定)                    = URL:KSIP Protocol
  URL Protocol              = （空文字）
  DefaultIcon
    (既定)                  = <ksip.exeのフルパス>,0
  shell\open\command
    (既定)                  = "<ksip.exeのフルパス>" "/ksip=%1"
```

パスは**書き込むたびに現在の実行ファイルの場所**にします。フォルダごと移動しても、移動後に一度起動すれば直ります。GPOなどで配布する場合は同じ内容を書いてください。その場合もパスは端末ごとの実際の場所に合わせる必要があります。

## リポジトリの構成

```text
.gitattributes       追跡する文字ファイルをどの環境でもCRLFで取り出す指定
.cargo/config.toml   Cargoの出力先と既定のrustflags
deps/                ネイティブ原本のロックと、取得したまま手を加えていない
                     ハッシュ固定済みアーカイブ。どちらもGit管理する
licenses/            Rust依存の補完ライセンス
src-web/             HTML・CSS・vanilla JavaScriptの画面
src-tauri/           Rust/Tauriのソースと固定設定
src-native/          KSIP固有のC/C++ソースとビルド定義
scripts/dev-env.ps1  MSVCツールチェーンとPATH。ビルド・テストの共通土台
scripts/deps/        固定版の依存を取得する。ネットワーク必須、依存更新時のみ
scripts/build/       取得済みの依存からビルドする。オフライン、毎回
scripts/test/        テスト。app-・asterisk-・loopback-で対向先を示す
```

ビルド用の一時生成物、テスト結果、キャッシュ、展開した依存ソースはすべて `temp/` に作られ、Git管理しません。

```text
release/             `ksip.exe` と `ksip-v<version>.exe`。運用時はここに履歴・録音・通知アイコンも増える
temp/build/          ビルド中間生成物とテスト用ファイル
temp/reports/        テスト・監査結果
temp/cargo-target/   Cargoキャッシュと中間生成物
temp/vendor/         固定アーカイブから展開する依存ソース
temp/w/・temp/d/     パス長を抑えたGoogle WebRTC・depot_tools
```

## ビルドに必要な環境

- Windows x64
- Visual Studio Build Tools 2022のC++ツールセット
- Windows SDK 10.0.28000.0とDebugging Tools for Windows
- Rust/Cargo
- Python 3.13
- cargo-audit（`cargo install cargo-audit --locked`）
- 実行時はMicrosoft WebView2 Runtime

## ビルド手順

この順で実行します。1・2は環境構築時と依存更新時だけ、3・4は毎回です。

```powershell
python -X utf8 scripts/deps/fetch-baresip.py
python -X utf8 scripts/deps/fetch-webrtc.py
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build/native.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build/app.ps1
```

`deps/fetch-baresip.py` はbaresip・re・Opus・libg722・LibreSSLの固定アーカイブをハッシュ検証します。
`deps/fetch-webrtc.py` は固定した公式Google WebRTCとdepot_tools revisionをパス長対策済みの `temp/w/`・`temp/d/` へ`gclient`で取得してDEPSも検証します。
`build/native.ps1` は内部で `build/webrtc.ps1` を呼びます。
`build/app.ps1` は `build/embed-notices.py` を呼んでから `release/` の `ksip.exe` と `ksip-v<version>.exe` を置き換えます。運用中のフォルダをそのままビルド先にできるよう、履歴や録音には触れません。配布物を作る場合は最後に `python -X utf8 scripts/build/package.py` を実行します。

`build/app.ps1` はビルド機のパスがバイナリへ残らないよう、`RUSTFLAGS` を組み立ててcargoへ渡します。これは `.cargo/config.toml` の `rustflags` を置き換えるため、静的CRTの指定も同じ場所に書いてあります。外部DLLへの依存が残れば `test/app-single-exe.ps1` が失敗します。

配布するexeは、どちらのスクリプトにも `-Clean` を付けて作ります。`native.ps1 -Clean` はCMakeのビルドディレクトリを消してから組み、以前の検出結果（古いキャッシュで `HAVE_LIBRESSL` が外れ、`http/server.c` の内容が変わった例があります）を引き継ぎません。`app.ps1 -Clean` は `cargo clean --release` を先に行います。

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build/native.ps1 -Clean
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build/app.ps1 -Clean
python -X utf8 scripts/test/build-paths.py
```

同じコミットをどの環境で組んでも同じexeになるように、ソースの改行は `.gitattributes` でCRLFに固定してあります。`src-web/` はそのままexeへ埋め込まれるため、取り出しの改行が違えば別のexeができます（Rust と C/C++ はコンパイラが改行を正規化するので影響しません）。`test/line-endings.py` が `src-web/` の改行を見ます。

## 主なテスト

テストの共通部品は `test/sip_fixture.py`（SIP端末の起動と制御、資格情報、通話確立）、`test/app_fixture.py`（使い捨てプロファイルと対向端末）、`test/app-fixture.ps1`・`test/ui-automation.ps1`（アプリの起動・終了とUIAutomation操作）です。製品のバージョンは `src-tauri/Cargo.toml` から読むため、版を上げてもテスト側の修正は要りません。

### 対向の要らないテスト

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test/rust.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test/aec.ps1
python -X utf8 scripts/test/audio-devices.py
python -X utf8 scripts/test/supply-chain.py
python -X utf8 scripts/test/i18n.py
python -X utf8 scripts/test/no-secrets.py
python -X utf8 scripts/test/build-paths.py
python -X utf8 scripts/test/line-endings.py
```

`test/aec.ps1` は通常、ABIとステレオサンプル数の契約に加え、80 ms遅延させた合成エコーを実製品と同じAPM経路へ入力し、抑圧量・ERL・ERLE・推定遅延をテストします。既定の通信スピーカーをADMで開くテストは `KSIP_TEST_AUDIO_DEVICE=1`、短い確認音がWindowsの出力へ実際に到達するテストは `KSIP_TEST_AUDIO_SIGNAL=1` を設定して実行します。

`test/audio-devices.py` にはWindowsの実音声デバイスが必要です。

`test/no-secrets.py` は、Gitが追跡しているファイルに私有IPアドレス・社内ホスト名・秘密らしき文字列・鍵や証明書の中身が混ざっていないかを調べます。開発用サーバーの接続先はリポジトリに置かず `local-asterisk/` から読む決まりで、この検査がそれを守ります。

`test/build-paths.py` は、ビルドした `release/ksip.exe` に組んだ機械の絶対パス（利用者のフォルダ、リポジトリの場所、cargoのレジストリ）が残っていないかを調べ、結果を `temp/reports/build-paths.json` に残します。`test/line-endings.py` は、`src-web/` の追跡ファイルが作業ツリーでCRLFであることを確かめます。改行がexeへ届くのはここだけです。どちらも数秒で終わります。

`test/i18n.py` は、本体とエンジンのKSIPモジュールが報告する名前を集め、画面の文言表とWindows用の表に過不足がないか、値を渡す名前と `{0}` を含む文言が対応しているかを検査します。対向も画面も要らず、数秒で終わります。

`test/supply-chain.py` はRustクレートの公開日とチェックサム、ネイティブ原本のハッシュとrevision、npm依存が無いことを検証し、続けて `cargo audit` で `Cargo.lock` のRustSec勧告を照会します。脆弱性が1件でもあれば失敗し、結果は `temp/reports/cargo-audit.json` に残します。unmaintained・unsoundの警告は記録だけして通します。

### ローカルの2プロセスだけで行うテスト

実音声デバイスは不要です。

```powershell
python -X utf8 scripts/test/loopback-call.py
python -X utf8 scripts/test/loopback-gain.py
python -X utf8 scripts/test/loopback-silent-mic.py
```
