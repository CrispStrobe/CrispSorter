"""Check the actual resolver with a filesystem-backed fake, without Cargo.
Requires an existing compatible anyhow rlib; never builds dependencies.
"""
import argparse, pathlib, subprocess, tempfile
args = argparse.ArgumentParser(description=__doc__)
args.add_argument('--anyhow', required=True, type=pathlib.Path)
args.add_argument('--source', type=pathlib.Path)
args.add_argument('--filter', default='')
args = args.parse_args()
root=pathlib.Path(__file__).resolve().parents[2]
s=(args.source or root/'src-tauri/src/drives/tauri_commands.rs').read_text()
def item(name,kind='fn'):
 start=s.index(f'{kind} {name}')
 end=s.index('\n}',start)+2
 return s[start:end]
pre='''use std::path::{Path,PathBuf};
#[derive(Clone,Copy,PartialEq)] enum ConflictPolicy { NewestWins,LocalWins,RemoteWins,Manual,KeepBoth }
#[derive(PartialEq)] enum DriveType { Local, WebDav }
struct FileStat { mtime_unix: Option<i64> }
trait CloudDrive { fn stat(&self,path:&Path)->anyhow::Result<FileStat>; fn drive_type(&self)->DriveType; }
'''
code=pre+item('MutationResolution','enum')+item('mutation_destination')
if 'fn mutation_path_exists' in s: code+=item('mutation_path_exists').replace('super::CloudDrive','CloudDrive')
code+=item('resolve_mutation_path').replace('super::CloudDrive','CloudDrive')
code+='''
struct Fault { root:PathBuf, kind:Option<std::io::ErrorKind>, remote:bool, fault:&'static str, source_stats:std::sync::atomic::AtomicUsize }
impl CloudDrive for Fault {
 fn drive_type(&self)->DriveType { if self.remote {DriveType::WebDav} else {DriveType::Local} }
 fn stat(&self,path:&Path)->anyhow::Result<FileStat> {
  if path==Path::new("source") { self.source_stats.fetch_add(1,std::sync::atomic::Ordering::Relaxed); }
  if path==Path::new(self.fault) { return match self.kind {
   Some(kind)=>Err(anyhow::Error::new(std::io::Error::new(kind,"lookup failed")).context("wrapped lookup")),
   None=>Err(anyhow::anyhow!("not found or unauthorized")) }; }
  std::fs::metadata(self.root.join(path))?;
  Ok(FileStat {mtime_unix:Some(if path==Path::new("source") {20} else {10})})
 }
}
fn fixture()->(PathBuf,Fault) {
 static NEXT:std::sync::atomic::AtomicUsize=std::sync::atomic::AtomicUsize::new(0);
 let root=std::env::temp_dir().join(format!("destination-check-{}-{}",std::process::id(),NEXT.fetch_add(1,std::sync::atomic::Ordering::Relaxed)));
 std::fs::create_dir(&root).unwrap();
 std::fs::write(root.join("source"),b"incoming").unwrap();
 std::fs::write(root.join("destination"),b"existing").unwrap();
 (root.clone(),Fault {root,kind:Some(std::io::ErrorKind::PermissionDenied),remote:false,fault:"destination",source_stats:std::sync::atomic::AtomicUsize::new(0)})
}
#[test] fn actual_resolver_blocks_filesystem_overwrite() {
 for moving in [false,true] { for kind in [std::io::ErrorKind::PermissionDenied,std::io::ErrorKind::TimedOut,std::io::ErrorKind::NotADirectory] {
 let (root,mut drive)=fixture(); drive.kind=Some(kind);
 let result=resolve_mutation_path(&drive,Path::new("source"),Path::new("destination"),ConflictPolicy::NewestWins);
 if let Ok(Some(destination))=&result {if moving {std::fs::rename(root.join("source"),root.join(destination)).unwrap();}else{std::fs::copy(root.join("source"),root.join(destination)).unwrap();}}
 assert_eq!(std::fs::read(root.join("destination")).unwrap(),b"existing","moving={moving}, kind={kind:?}");
 assert!(result.is_err());
 assert_eq!(drive.source_stats.load(std::sync::atomic::Ordering::Relaxed),0);
 std::fs::remove_dir_all(root).unwrap();
 }}
}
#[test] fn other_policies_and_keep_both_local_faults() {
 for policy in [ConflictPolicy::LocalWins,ConflictPolicy::RemoteWins,ConflictPolicy::Manual,ConflictPolicy::KeepBoth] {
 let(root,mut drive)=fixture();
 assert!(resolve_mutation_path(&drive,Path::new("source"),Path::new("destination"),policy).is_err());
 drive.fault="destination (copy)";
 assert!(resolve_mutation_path(&drive,Path::new("source"),Path::new("destination"),ConflictPolicy::KeepBoth).is_err());
 std::fs::remove_dir_all(root).unwrap();
 }
}
#[test] fn genuine_missing_and_existing_and_legacy_remote_controls() {
 let(root,mut drive)=fixture(); drive.fault="no-fault";
 assert_eq!(resolve_mutation_path(&drive,Path::new("source"),Path::new("absent"),ConflictPolicy::NewestWins).unwrap(),Some("absent".into()));
 assert_eq!(resolve_mutation_path(&drive,Path::new("source"),Path::new("destination"),ConflictPolicy::NewestWins).unwrap(),Some("destination".into()));
 assert_eq!(resolve_mutation_path(&drive,Path::new("source"),Path::new("destination"),ConflictPolicy::LocalWins).unwrap(),None);
 assert!(resolve_mutation_path(&drive,Path::new("source"),Path::new("destination"),ConflictPolicy::Manual).is_err());
 assert_eq!(resolve_mutation_path(&drive,Path::new("source"),Path::new("destination"),ConflictPolicy::KeepBoth).unwrap(),Some("destination (copy)".into()));
 drive.fault="destination"; drive.kind=None;
 assert!(resolve_mutation_path(&drive,Path::new("source"),Path::new("destination"),ConflictPolicy::NewestWins).unwrap().is_some());
 drive.kind=Some(std::io::ErrorKind::PermissionDenied); drive.remote=true;
 assert!(resolve_mutation_path(&drive,Path::new("source"),Path::new("destination"),ConflictPolicy::NewestWins).unwrap().is_some());
 drive.kind=Some(std::io::ErrorKind::NotFound);
 assert!(resolve_mutation_path(&drive,Path::new("source"),Path::new("destination"),ConflictPolicy::NewestWins).unwrap().is_some());
 drive.remote=false;
 assert!(resolve_mutation_path(&drive,Path::new("destination"),Path::new("destination"),ConflictPolicy::NewestWins).unwrap().is_some());
 std::fs::remove_dir_all(root).unwrap();
}
'''
with tempfile.TemporaryDirectory(prefix='destination-lookup-build-') as output:
    path=pathlib.Path(output)/'check.rs'
    path.write_text(code)
    binary=pathlib.Path(output)/'check'
    rlib=args.anyhow.resolve()
    subprocess.run(['rustc','--edition=2021','--test',str(path),'--extern',f'anyhow={rlib}','-L',f'dependency={rlib.parent}','-C','debuginfo=0','-o',str(binary)],check=True)
    raise SystemExit(subprocess.run([str(binary),'--test-threads=1',args.filter]).returncode)
