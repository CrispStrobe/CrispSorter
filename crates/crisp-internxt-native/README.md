# crisp-internxt-native

Reusable native Internxt Cloud Drive protocol client for CrispSorter.

The crate owns Internxt login transport, session serialization, folder/file
operations, and file encryption. CrispSorter adds its keychain and
`CloudDrive` adapter around it. The `crisp-internxt` binary exercises the same
library without starting Tauri.

Build and run the offline protocol check:

```sh
cargo run -p crisp-internxt-native -- crypto-vector
```

For live testing, provide the password through stdin and choose an explicit
session path. Session files contain bearer credentials and the account
mnemonic; protect or remove them after testing:

```sh
printf '%s\n' "$INTERNXT_PASSWORD" \
  | cargo run -p crisp-internxt-native -- login user@example.com --session /tmp/internxt-session.json
cargo run -p crisp-internxt-native -- list /tmp/internxt-session.json .
```
