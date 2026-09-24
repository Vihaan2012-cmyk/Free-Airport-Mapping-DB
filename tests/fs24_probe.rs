//! Does the archive reader actually open this machine's FS2024 navigation data?
use amdbgen::sources::msfs::fsarchive::FsArchive;
use std::path::PathBuf;

#[test]
#[ignore]
fn opens_the_real_fs24_navdata() {
    let p = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap())
        .join(r"Packages\Microsoft.Limitless_8wekyb3d8bbwe\LocalCache\Packages\StreamedPackages\fs24-fs-base-nav\content\minimal.fsarchive");
    let a = FsArchive::open(&p).expect("open the archive");
    println!("entries: {}", a.len());
    let first: Vec<&str> = a.paths().take(3).collect();
    println!("first paths: {first:?}");
    let name = a.paths().find(|p| p.ends_with("atx00000.bgl")).unwrap().to_string();
    let bytes = a.read(&name).expect("read a bgl");
    println!("{name}: {} bytes, first 8 = {:02x?}", bytes.len(), &bytes[..8]);
    assert_eq!(&bytes[..4], &[0x01, 0x02, 0x92, 0x19], "a real BGL header");
}
