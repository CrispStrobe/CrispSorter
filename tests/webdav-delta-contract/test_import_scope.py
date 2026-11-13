"""Compile production streaming reads with their own std imports, without Cargo."""

from pathlib import Path
import re
import subprocess
import tempfile
import unittest


SOURCE = Path(__file__).resolve().parents[2] / "src-tauri/src/drives/webdav.rs"


def production_source():
    source = SOURCE.read_text()
    # Keep the actual parent module's std imports, including grouped/multiline
    # declarations. The harness must never provide its own unqualified Write.
    module_scope = source[:source.index("pub struct WebDavDrive")]
    imports = "\n".join(re.findall(r"^use std::[^;]+;", module_scope, re.MULTILINE))
    start = source.index("    fn read_file_to_writer(")
    end = source.index("\n    /// Read an inclusive byte range", start)
    return imports, source[start:end]


# Fake only the surrounding application/HTTP APIs. All std::io traits in these
# helpers are fully qualified so they cannot mask a missing production import.
FIXTURE = r'''
#![allow(dead_code, unused_imports)]
mod production {
    __IMPORTS__
    type Result<T> = std::result::Result<T, String>;
    trait Context<T> {
        fn with_context(self, context: impl FnOnce() -> String) -> Result<T>;
    }
    impl<T, E: std::fmt::Display> Context<T> for std::result::Result<T, E> {
        fn with_context(self, context: impl FnOnce() -> String) -> Result<T> {
            self.map_err(|error| format!("{}: {error}", context()))
        }
    }
    macro_rules! anyhow {
        ($($arg:tt)*) => { format!($($arg)*) };
    }
    mod reqwest {
        pub enum Method { GET }
    }
    struct WebDavDrive { status: u16 }
    struct Request { status: u16 }
    struct Response { status: u16, body: std::io::Cursor<Vec<u8>> }
    struct Status(u16);
    impl Status {
        fn is_success(&self) -> bool { (200..300).contains(&self.0) }
    }
    impl std::fmt::Display for Status {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }
    impl Request {
        fn send(self) -> Result<Response> {
            Ok(Response { status: self.status, body: std::io::Cursor::new(b"streamed bytes".to_vec()) })
        }
    }
    impl Response {
        fn status(&self) -> Status { Status(self.status) }
        fn text(self) -> Result<String> { Ok("denied".into()) }
    }
    impl std::io::Read for Response {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            std::io::Read::read(&mut self.body, buf)
        }
    }
    impl WebDavDrive {
        fn url_for(&self, _: &std::path::Path) -> String { "https://fixture/file".into() }
        fn req(&self, _: reqwest::Method, _: &str) -> Request { Request { status: self.status } }
        __METHOD__
    }
    #[test]
    fn production_stream_copies_bytes_and_reports_count() {
        let mut output = Vec::new();
        let count = WebDavDrive { status: 200 }
            .read_file_to_writer(std::path::Path::new("file"), &mut output).unwrap();
        assert_eq!(output, b"streamed bytes");
        assert_eq!(count, output.len() as u64);
    }
    #[test]
    fn production_stream_rejects_http_failure_before_writing() {
        let mut output = Vec::new();
        let error = WebDavDrive { status: 403 }
            .read_file_to_writer(std::path::Path::new("file"), &mut output).unwrap_err();
        assert!(error.contains("403: denied"), "{error}");
        assert!(output.is_empty());
    }
    struct BrokenWriter;
    impl std::io::Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("writer failed"))
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    #[test]
    fn production_stream_preserves_writer_error_with_context() {
        let error = WebDavDrive { status: 200 }
            .read_file_to_writer(std::path::Path::new("file"), &mut BrokenWriter).unwrap_err();
        assert!(error.contains("streaming GET https://fixture/file: writer failed"), "{error}");
    }
}
'''


class ProductionImportScope(unittest.TestCase):
    def compile(self, imports, method, directory):
        source = Path(directory) / "streaming.rs"
        binary = Path(directory) / "streaming-test"
        source.write_text(FIXTURE.replace("__IMPORTS__", imports).replace("__METHOD__", method))
        result = subprocess.run(
            ["rustc", "--edition=2021", "--test", str(source), "-o", str(binary)],
            capture_output=True, text=True,
        )
        return result, binary

    def test_real_method_compiles_and_streams_with_production_imports(self):
        imports, method = production_source()
        with tempfile.TemporaryDirectory(prefix="webdav-import-scope-") as directory:
            result, binary = self.compile(imports, method, directory)
            self.assertEqual(result.returncode, 0, result.stderr)
            result = subprocess.run([str(binary)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_missing_write_import_is_a_compile_error(self):
        imports, method = production_source()
        # Reproduce the release regression without changing the repository.
        broken_imports, removed = re.subn(r",\s*Write\b|\bWrite\s*,\s*", "", imports)
        self.assertEqual(removed, 1, "negative control must remove the production Write import")
        with tempfile.TemporaryDirectory(prefix="webdav-import-scope-") as directory:
            result, _ = self.compile(broken_imports, method, directory)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("error[E0405]", result.stderr)
            self.assertIn("cannot find trait `Write`", result.stderr)


if __name__ == "__main__":
    unittest.main()
