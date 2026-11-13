`native-dbnet-trocr.stderr.txt` is retained stderr from the successful packaged
Linux DBNet/TrOCR fixture control associated with original app source `b739a3c0`
and native source `c364feb9`. Operation UUIDs are replaced with `<operation-id>`;
no local paths or hosted execution identifiers are included. This exercises real
native log syntax such as `[info]`, `[graph]`, `[:5]`, `[0..4]` and whitespace
numeric vectors alongside JSON operation records.

The fixture is a sanitizer compatibility regression. Replaying these bytes from
a test subprocess is not native execution or macOS/quality qualification.

`doctor.stdout.json` is actual sanitized doctor stdout retained by the reviewed
macOS replay. It contains the source-defined eight booleans plus `lance_dir`;
the isolated-home prefix is already replaced with `<local-path>`. It is schema
evidence, not an independent execution or package qualification attestation.
