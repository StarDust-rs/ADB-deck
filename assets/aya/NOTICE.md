# Aya helper (from ADBPlus)

`aya.dex` is the on-device helper of **ADBPlus** (https://github.com/danweiyuancircle/adb-plus,
version 1.20.0, commit f184970854cc5cb89a2d776f4632f6dd420bfd44), itself a fork of **AYA** by
liriliri (https://github.com/liriliri/aya, where the `io.liriliri.aya` helper originates).
Copyright belongs to their authors; the code is licensed under the **GNU Affero General
Public License, version 3** (`LICENSE-AGPL-3.0.txt`, taken from the ADBPlus repository).

ADB Deck pushes it to `/data/local/tmp/aya/aya.dex` on the device and starts it with
`app_process`, exactly as ADBPlus does. It answers `getPackageInfos` with each app's label and
a PNG of its launcher icon rendered by Android itself.

* `source/` holds the helper's complete Kotlin/Protobuf source and Gradle build files as
  published by ADBPlus. It is the corresponding source for `aya.dex`.
* `aya.dex` is the prebuilt binary taken unmodified from ADBPlus' `resources/aya.dex`
  (SHA-256 `68f5378e4d8a66c34b9a6c24d1fa64662efb59075242a52d56fea06f87da99f9`).
  It was **not** rebuilt here; that it was built from this exact source is taken from
  ADBPlus and has not been verified.
* `src/aya.rs` is a Rust port of the client side of ADBPlus' `src/main/lib/adb/server.ts`
  (connect to `localabstract:aya`, length-delimited Protobuf `Request`/`Response` carrying JSON
  `params`/`result`, push + `app_process` start-up, `getPackageInfos`). It is therefore a
  derivative work and is covered by the AGPL-3.0 as well.

Differences from ADBPlus:

* The helper's HTTP file server (`startFileServer`) is never started. It listens on every
  network interface of the phone without authentication; icon PNGs are read with
  `adb exec-out cat` instead.
* Replies are accumulated until a whole message has arrived, and requests time out.
* The helper is stopped when the device panel is closed.
