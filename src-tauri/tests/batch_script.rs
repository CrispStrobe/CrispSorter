// Script execution is tested outside src so the sandbox source guard can
// continue scanning all production modules without an exemption.
#[path = "../src/batch_script.rs"]
mod batch_script;

#[cfg(test)]
mod tests {
    use super::batch_script::Script;

    #[test]
    fn powershell_paths_are_single_quoted_and_escaped() {
        let mut script = Script::new(true);
        script
            .add("C:\\odd'$env:TEST`name.pdf", "C:\\out%TEMP%!'.pdf", true)
            .unwrap();
        let text = script.finish();
        assert!(
            text.contains("Sort-File 'C:\\odd''$env:TEST`name.pdf' 'C:\\out%TEMP%!''.pdf' $true")
        );
        assert!(text.contains("[System.IO.FileMode]::CreateNew"));
        assert!(text.contains("exit 1"));
    }

    #[test]
    fn rejects_nul_paths() {
        assert!(Script::new(false).add("a\0b", "dest", true).is_err());
        assert!(Script::new(true).add("src", "a\0b", false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn shell_move_and_missing_source_are_reported_correctly() {
        use std::{
            fs,
            process::Command,
            sync::atomic::{AtomicUsize, Ordering},
        };
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "crisp-script-move-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let source = dir.join("source.pdf");
        let dest = dir.join("dest.pdf");
        let missing_dest = dir.join("missing-dest.pdf");
        fs::write(&source, b"data").unwrap();
        let mut script = Script::new(false);
        script
            .add(source.to_str().unwrap(), dest.to_str().unwrap(), true)
            .unwrap();
        script
            .add(
                dir.join("missing.pdf").to_str().unwrap(),
                missing_dest.to_str().unwrap(),
                false,
            )
            .unwrap();
        let script_path = dir.join("run.sh");
        fs::write(&script_path, script.finish()).unwrap();
        let output = Command::new("bash").arg(&script_path).output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("ERROR:"));
        assert!(!source.exists());
        assert_eq!(fs::read(dest).unwrap(), b"data");
        assert!(!missing_dest.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn shell_script_handles_literal_paths_and_reports_collisions() {
        use std::{
            fs,
            process::Command,
            sync::atomic::{AtomicUsize, Ordering},
        };
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "crisp-script-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let source = dir.join("source ' $HOME $(touch injected)\n.pdf");
        let destination = dir.join("sub ' $HOME $(touch injected)\n/dest.pdf");
        let collision = dir.join("existing.pdf");
        let other = dir.join("other.pdf");
        fs::write(&source, b"original").unwrap();
        fs::write(&collision, b"keep").unwrap();
        fs::write(&other, b"other").unwrap();
        let mut script = Script::new(false);
        script
            .add(
                source.to_str().unwrap(),
                destination.to_str().unwrap(),
                false,
            )
            .unwrap();
        script
            .add(other.to_str().unwrap(), collision.to_str().unwrap(), true)
            .unwrap();
        let script_path = dir.join("run.sh");
        fs::write(&script_path, script.finish()).unwrap();
        let output = Command::new("bash")
            .arg(&script_path)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("ERROR:"));
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        assert_eq!(fs::read(&collision).unwrap(), b"keep");
        assert_eq!(fs::read(&other).unwrap(), b"other");
        assert!(!dir.join("injected").exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn production_generator_has_no_process_launcher() {
        let source = include_str!("../src/batch_script.rs");
        assert!(!source.contains("Command::new"));
        assert!(source.contains("[System.IO.FileMode]::CreateNew"));
        assert!(source.contains("set -C; cat"));
    }
}
