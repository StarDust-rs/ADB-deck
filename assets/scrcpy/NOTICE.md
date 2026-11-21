# scrcpy server

`scrcpy-server-v3.1.jar` is the on-device server of **scrcpy** (https://github.com/Genymobile/scrcpy),
version 3.1, copyright Genymobile and Romain Vimont, licensed under the **Apache License 2.0**
(`LICENSE-Apache-2.0.txt`). It is the file published as `scrcpy-server-v3.1` on the project's
GitHub release page for v3.1 and is bundled **unmodified**.

* SHA-256 of the bundled file: `958f0944a62f23b1f33a16e9eb14844c1a04b882ca175a738c16d23cb22b86c0`
  (computed after download; it has not been compared with a checksum published by the scrcpy
  project, because none was fetched).
* Corresponding source: tag `v3.1` of the repository above (directory `server/`).
* ADB Deck pushes it to `/data/local/tmp/adbdeck/scrcpy-server.jar` and starts it with
  `app_process`, as scrcpy itself and ADBPlus do. The protocol client is `src/mirror/`; it
  follows the server source of that tag (`DesktopConnection`, `Streamer`, `ControlMessageReader`,
  `DeviceMessageWriter`, `Options`) and was written for this project.
* The server version and the client must match exactly, so the jar is bundled, not looked up.
