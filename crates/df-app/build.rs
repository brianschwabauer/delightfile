//! The Windows program's icon and version resource
//! (`plans/other-platforms/06-build-and-release.md` B6.20), and nothing on
//! any other target.
//!
//! `build/windows/delightfile.rc` holds the resources. This compiles it with
//! the Windows SDK's `rc.exe` (or `llvm-rc`, or whatever `RC` names) into a
//! `.res` in `OUT_DIR` and hands that to the linker for the `delightfile`
//! binary alone, which `link.exe` takes as it is. No resource crate: the whole
//! job is one command and one linker argument, and every Windows machine that
//! can build the program has the SDK the MSVC toolchain needs anyway.
//!
//! The version comes from Cargo, through a header this writes beside the
//! `.res`, so Explorer's Properties > Details and `--version` cannot disagree.
//! The icon is `build/windows/delightfile.ico`, which `build/windows/icon.ps1`
//! draws from the SVG; it is generated rather than committed, so a build
//! without it has the version and no icon, and says so when it is a release
//! build. The release workflow draws it first.
//!
//! A check or clippy for Windows from another OS (`build/xcheck.sh`) links
//! nothing and has no resource compiler, so there this does nothing.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var_os("CARGO_CFG_WINDOWS").is_none() || !cfg!(windows) {
        return Ok(());
    }

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let resources = manifest.join("../../build/windows");
    let out = PathBuf::from(env::var("OUT_DIR")?);
    // The folder, not its files, so an icon that appears later is noticed.
    println!("cargo:rerun-if-changed={}", resources.display());
    println!("cargo:rerun-if-env-changed=RC");

    let icon = resources.join("delightfile.ico").is_file();
    if !icon && env::var("PROFILE").as_deref() == Ok("release") {
        println!(
            "cargo:warning=no build/windows/delightfile.ico, so delightfile.exe has no icon; \
             run build/windows/icon.ps1 first"
        );
    }
    let version = env::var("CARGO_PKG_VERSION")?;
    let mut header = String::new();
    for (name, var) in [
        ("DF_VERSION_MAJOR", "CARGO_PKG_VERSION_MAJOR"),
        ("DF_VERSION_MINOR", "CARGO_PKG_VERSION_MINOR"),
        ("DF_VERSION_PATCH", "CARGO_PKG_VERSION_PATCH"),
    ] {
        header.push_str(&format!("#define {name} {}\n", env::var(var)?));
    }
    header.push_str(&format!("#define DF_VERSION \"{version}\"\n"));
    if icon {
        header.push_str("#define DF_ICON 1\n");
    }
    fs::write(out.join("delightfile-version.h"), header)?;

    let res = out.join("delightfile.res");
    let compiler = resource_compiler().ok_or(
        "no resource compiler: the Windows SDK's rc.exe was not found under \
         Windows Kits\\10\\bin, and neither rc.exe nor llvm-rc is on PATH (RC can name one)",
    )?;
    // Run from the resources' folder, where rc looks for the icon; the
    // header is found through /i.
    let status = Command::new(&compiler)
        .current_dir(&resources)
        .arg("/nologo")
        .arg("/i")
        .arg(&out)
        .arg("/fo")
        .arg(&res)
        .arg("delightfile.rc")
        .status()?;
    if !status.success() {
        return Err(format!("{} failed on delightfile.rc: {status}", compiler.display()).into());
    }
    println!("cargo:rustc-link-arg-bin=delightfile={}", res.display());
    Ok(())
}

/// `RC` if set; `rc.exe` on `PATH`, as in a Visual Studio prompt; the newest
/// Windows 10/11 SDK's x64 `rc.exe`; `llvm-rc` on `PATH`.
fn resource_compiler() -> Option<PathBuf> {
    if let Some(rc) = env::var_os("RC") {
        return Some(PathBuf::from(rc));
    }
    if let Some(rc) = on_path("rc.exe") {
        return Some(rc);
    }
    let kits = env::var_os("ProgramFiles(x86)")
        .map(|dir| PathBuf::from(dir).join("Windows Kits/10/bin"))
        .and_then(|kits| fs::read_dir(kits).ok());
    let newest = kits
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path().join("x64/rc.exe"))
        .filter(|rc| rc.is_file())
        .max_by_key(|rc| sdk_version(rc));
    newest.or_else(|| on_path("llvm-rc.exe"))
}

/// The SDK folder's version (`10.0.26100.0`) as numbers, for picking the
/// newest; a folder that is not a version sorts first.
fn sdk_version(rc: &Path) -> Vec<u32> {
    rc.ancestors()
        .nth(2)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(|name| name.split('.').filter_map(|n| n.parse().ok()).collect())
        .unwrap_or_default()
}

fn on_path(program: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
}
