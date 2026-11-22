//! Repackages the GDB from xPack's GNU Arm Embedded GCC releases.
//!
//! It's difficult to install GDB on some platforms (namely Windows, where one of the easiest ways
//! is through MinGW) and often requires admin permissions, which can make it difficult for
//! students to take advantage of v5gdb (which depends on GDB proper). PROS ships GDB, but it's an
//! old version without DAP support and thus can't be used by VS Code. A nice alternative is the GDB
//! from the xPack Embedded ARM toolchain, which is relocatable and modern, but those toolchains
//! are fairly large - the compressed download is ~250 MB. This module contains functions for
//! repacking the xPack toolchain to only contain the things that v5gdb actually uses, i.e. GDB and
//! its dependencies. Everything from xPack is copied into the repack without modification.

use std::{
    collections::{BTreeSet, HashMap},
    fs::{self, File},
    io::{self, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use digest_io::IoWrapper;
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use goblin::{Object, mach::Mach};
use sha2::{Digest, Sha256};
use ureq::http::header::USER_AGENT;

/// The xPack GNU Arm Embedded GCC release that GDB is taken from.
const XPACK_VERSION: &str = "15.2.1-1.1";
/// This allows us to publish new releases of the repack without updating xPack.
const REVISION: u32 = 1;

const XPACK_URL: &str = "https://github.com/xpack-dev-tools/arm-none-eabi-gcc-xpack";

/// xPack's archives.
const PLATFORMS: &[Platform] = &[
    Platform {
        name: "darwin-arm64",
        sha256: "574082d35e49a2bcbdc355836b2a3ae5e5bb3b9456c9f5e37177db2ab4aad870",
    },
    Platform {
        name: "darwin-x64",
        sha256: "5be906a5194c3173e145a58048e5607ffff947773237802b93e55e23c415f1b6",
    },
    Platform {
        name: "linux-arm64",
        sha256: "67980c7990eba7bb7ffdf39699102effd70889f5ac427be19a8c8a6c5fab2972",
    },
    Platform {
        name: "linux-x64",
        sha256: "da6a49ad4003944b823c6c93702a8787c922ab34bd7e918ec0eaf6933a9b1ff6",
    },
    Platform {
        name: "win32-x64",
        sha256: "bae6a3d1667697ce750c3b13d6d26d80973ecedc2cc87bf04869e83447fd93ea",
    },
];

/// Parts of the Python standard library that GDB doesn't need.
const EXCLUDED_PYTHON_DIRS: &[&str] = &["test", "idlelib", "tkinter", "turtledemo", "ensurepip"];

/// This is the version of the Python license we have checked in.
const WINDOWS_PYTHON_VERSION: &str = "3.13.12";
/// There's a bug in upstream xPack that means this doesn't get included, so we have to add it in
/// manually.
const WINDOWS_PYTHON_LICENSE: &[u8] = include_bytes!("../assets/python-win-LICENSE.txt");

struct Platform {
    /// Platform name used in xPack release bundle.
    name: &'static str,
    /// Hash of this xPack release bundle.
    sha256: &'static str,
}

impl Platform {
    fn is_windows(&self) -> bool {
        self.name.starts_with("win32")
    }

    fn archive_ext(&self) -> &'static str {
        if self.is_windows() { "zip" } else { "tar.gz" }
    }

    fn xpack_file(&self) -> String {
        format!(
            "xpack-arm-none-eabi-gcc-{XPACK_VERSION}-{}.{}",
            self.name,
            self.archive_ext()
        )
    }

    fn gdb_exe(&self) -> &'static str {
        if self.is_windows() {
            "bin/arm-none-eabi-gdb-py3.exe"
        } else {
            "bin/arm-none-eabi-gdb-py3"
        }
    }

    /// Get all platforms with names in `platforms`. If `platforms` is empty, returns all platforms.
    fn filter(platforms: &[String]) -> Result<Vec<&Self>> {
        if platforms.is_empty() {
            Ok(PLATFORMS.iter().collect())
        } else {
            platforms
                .iter()
                .map(|name| {
                    PLATFORMS
                        .iter()
                        .find(|p| p.name == name)
                        .with_context(|| format!("xPack has no known GDB build for {name}"))
                })
                .collect::<Result<_>>()
        }
    }

    /// Downloads this xPack archive from GitHub (if needed), then checks its SHA-256.
    fn download(&self, dir: &Path) -> Result<PathBuf> {
        let file: String = self.xpack_file();
        let path = dir.join(&file);
        if path.exists() && sha256_file(&path)? == self.sha256 {
            return Ok(path);
        }

        let url = format!("{XPACK_URL}/releases/download/v{XPACK_VERSION}/{file}");
        println!("Downloading {url}");
        let partial = dir.join(format!("{file}.partial"));

        let mut body = ureq::get(&url)
            .header(USER_AGENT, "v5gdb-xtask (https://github.com/vexide/v5gdb)")
            .call()?
            .into_body()
            .into_reader();
        let mut dest = BufWriter::new(File::create(&partial)?);
        io::copy(&mut body, &mut dest)?;

        let hash = sha256_file(&partial)?;
        if hash != self.sha256 {
            bail!("{file} has SHA-256 {hash}, expected {}", self.sha256);
        }
        fs::rename(&partial, &path)?;
        Ok(path)
    }

    fn repack(
        &self,
        downloads_dir: &Path,
        dest_dir: &Path,
        shasums: &mut String,
    ) -> Result<ComponentVersions> {
        let archive = self.download(downloads_dir)?;

        let work_dir = downloads_dir.join("../tmp").join(self.name);
        _ = fs::remove_dir_all(&work_dir);
        fs::create_dir_all(&work_dir)?;

        println!("Extracting {}", archive.display());
        let extracted_files = if self.is_windows() {
            let mut files = extract_zip(&archive, &work_dir)?;
            add_windows_python_license(&work_dir, &mut files)?;
            files
        } else {
            extract_tar(&archive, &work_dir)?
        };

        let files = self.select_files(&work_dir, &extracted_files)?;
        let component_versions = ComponentVersions::detect(&work_dir)?;
        let readme = self.readme(&component_versions);

        let bundle_name = format!("arm-none-eabi-gdb-{}-{}", version(), self.name);
        let bundle_filename = format!("{bundle_name}.{}", self.archive_ext());
        let bundle = NewBundle {
            path: dest_dir.join(&bundle_filename),
            name: bundle_name,
            src_dir: &work_dir,
            file_paths: &files,
            readme: &readme,
        };

        println!("Bundling {} ({} files)", bundle.path.display(), files.len());
        if self.is_windows() {
            bundle.write_zip()?;
        } else {
            bundle.write_tar()?;
        }

        let hash = sha256_file(&bundle.path)?;
        *shasums += &format!("{hash}  {bundle_filename}\n");
        fs::remove_dir_all(&work_dir)?;

        Ok(component_versions)
    }

    /// Returns the files to keep: the roots, and every shared library they load (transitively) that
    /// is part of the archive.
    fn select_files(&self, work: &Path, extracted: &[String]) -> Result<BTreeSet<String>> {
        // Lowercase because Windows DLL names are case-insensitive.
        let by_lower_path: HashMap<String, &str> = extracted
            .iter()
            .map(|rel| (rel.to_lowercase(), rel.as_str()))
            .collect();

        let mut selected = BTreeSet::new();
        let mut queue: Vec<String> = extracted
            .iter()
            .filter(|rel| is_root_file(self, rel))
            .cloned()
            .collect();
        if !queue.iter().any(|rel| rel == self.gdb_exe()) {
            bail!("{} isn't in the archive", self.gdb_exe());
        }

        // Discover dependencies.
        while let Some(rel) = queue.pop() {
            if !selected.insert(rel.clone()) {
                continue;
            }
            let path = work.join(&rel);
            let dir = rel.rsplit_once('/').map_or("", |(dir, _)| dir);

            if path.symlink_metadata()?.is_symlink() {
                let target = format!("{dir}/{}", fs::read_link(&path)?.display());
                // This could also trigger if a symlink does something like `../lib/foo`.
                if !by_lower_path.contains_key(&target.to_lowercase()) {
                    bail!("{rel} links to {target}, which wasn't extracted");
                }
                queue.push(target);
                continue;
            }

            for lib in linked_libraries(&path)? {
                // To make things a bit simpler, we only care about the filename. This helps us deal
                // with Mach-O paths which have a prefix like @rpath/...
                let filename = lib.rsplit('/').next().unwrap();
                let found = [dir, "libexec", "bin", "bin/DLLs"]
                    .iter()
                    .find_map(|d| by_lower_path.get(&format!("{d}/{filename}").to_lowercase()));
                // If we didn't find a dylib in one of the known locations, it's probably an OS
                // library like libc.
                if let Some(found) = found {
                    queue.push(found.to_string());
                }
            }
        }
        Ok(selected)
    }

    /// Generates the README for a repack.
    fn readme(&self, versions: &ComponentVersions) -> String {
        let ComponentVersions { gdb, python } = versions;
        let exe = self.gdb_exe();
        format!(
            "\
# arm-none-eabi GDB for v5gdb

Repack of GDB {gdb} for `arm-none-eabi` with Python {python}, built by the xPack project and taken
unmodified from the xPack GNU Arm Embedded GCC {XPACK_VERSION} release for {platform}.

- v5gdb: <https://github.com/vexide/v5gdb>
- Base xPack release: <{XPACK_URL}/releases/tag/v{XPACK_VERSION}>

## Contents

This repack is stripped to only contain the files needed to run `{exe}`, including:

- `{exe}`
- GDB's shared library dependencies, including Python {python}
- the Python standard library (without tests, IDLE, Tkinter, turtledemo, ensurepip)
- GDB's data directory (`arm-none-eabi/share/gdb`)
- `distro-info/`, which contains licenses

Other parts of the xPack toolchain, like compilers, binutils, newlib, etc. were removed. Files have
not been changed.

## Source code and licenses

GDB is free software, licensed under the GNU General Public License version 3 or later.
Licenses for any tools included in the xPack toolchain can be found in `distro-info/licenses/`.
",
            platform = self.name,
        )
    }
}

/// The version of the repackaged GDB, as used in the release tag (`gdb-v<version>`).
pub fn version() -> String {
    format!("{XPACK_VERSION}-r{REVISION}")
}

pub fn repack(out_dir: &Path, platforms: &[String]) -> Result<()> {
    let selected = Platform::filter(platforms)?;

    let downloads = out_dir.join("downloads");
    fs::create_dir_all(&downloads)?;
    let dist = out_dir.join("dist");
    _ = fs::remove_dir_all(&dist);
    fs::create_dir_all(&dist)?;

    let mut sums = String::new();
    let mut versions = None;
    for platform in selected {
        versions = Some(platform.repack(&downloads, &dist, &mut sums)?);
    }

    fs::write(dist.join("SHA256SUMS"), &sums)?;
    if let Some(versions) = versions {
        fs::write(
            out_dir.join("release-notes.md"),
            release_notes(&versions, &sums),
        )?;
    }
    print!("\n{sums}");
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut hasher = IoWrapper(Sha256::new());
    io::copy(&mut File::open(path)?, &mut hasher)?;
    let hash = hasher.0.finalize();
    Ok(hash.iter().map(|b| format!("{b:02x}")).collect())
}

/// Adds Python's license to xPack's Windows build, which leaves it out.
fn add_windows_python_license(work: &Path, extracted: &mut Vec<String>) -> Result<()> {
    if extracted
        .iter()
        .any(|rel| rel.starts_with("distro-info/licenses/python-"))
    {
        return Ok(());
    }

    // Check that our license actually matches the version of Python xPack is using.
    let mut parts = WINDOWS_PYTHON_VERSION.split('.');
    if !extracted.contains(&format!(
        "bin/python{}{}.dll",
        parts.next().unwrap(),
        parts.next().unwrap()
    )) {
        bail!(
            "xPack's Windows build doesn't have Python {WINDOWS_PYTHON_VERSION}; update \
             WINDOWS_PYTHON_VERSION and xtask/assets/python-win-LICENSE.txt"
        );
    }

    let rel = format!("distro-info/licenses/python-{WINDOWS_PYTHON_VERSION}/LICENSE.txt");
    let out = work.join(&rel);
    fs::create_dir_all(out.parent().unwrap())?;
    fs::write(out, WINDOWS_PYTHON_LICENSE)?;
    extracted.push(rel);
    Ok(())
}

/// Removes the top-level directory from an archive path.
fn strip_top_dir(path: &str) -> Option<&str> {
    path.split_once('/')
        .map(|(_, rest)| rest)
        .filter(|rest| !rest.is_empty())
}

/// Whether a file might be needed in a repack. This avoids extracting compilers, but isn't as
/// granular as `select_files`.
fn maybe_useful_in_repack(rel: &str) -> bool {
    if let Some(name) = rel.strip_prefix("bin/") {
        // The other `arm-none-eabi-*` programs are binutils, GCC, and non-Python GDB.
        return !name.starts_with("arm-none-eabi-") || rel.starts_with("bin/arm-none-eabi-gdb-py3");
    }
    (rel.starts_with("libexec/") && !rel.starts_with("libexec/gcc/"))
        || rel.starts_with("lib/python3")
        || rel.starts_with("arm-none-eabi/share/gdb/")
        || rel.starts_with("distro-info/")
}

/// Extracts the candidate files of a `.tar.gz` into `dest`, returning their relative paths.
fn extract_tar(archive: &Path, dest: &Path) -> Result<Vec<String>> {
    let mut tar = tar::Archive::new(GzDecoder::new(BufReader::new(File::open(archive)?)));
    let mut extracted = Vec::new();
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();
        let Some(rel) = strip_top_dir(&path) else {
            continue;
        };
        if entry.header().entry_type().is_dir() || !maybe_useful_in_repack(rel) {
            continue;
        }
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_symlink()) {
            bail!("{path}: unsupported tar entry type {kind:?}");
        }
        let out = dest.join(rel);
        fs::create_dir_all(out.parent().unwrap())?;
        entry.set_preserve_permissions(true);
        entry.unpack(&out)?;
        extracted.push(rel.to_owned());
    }
    Ok(extracted)
}

/// Extracts the candidate files of a `.zip` into `dest`, returning their relative paths.
fn extract_zip(archive: &Path, dest: &Path) -> Result<Vec<String>> {
    let mut zip = zip::ZipArchive::new(BufReader::new(File::open(archive)?))?;
    let mut extracted = Vec::new();
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        if file.is_dir() {
            continue;
        }

        let path = file
            .enclosed_name()
            .with_context(|| format!("unsafe path in archive: {:?}", file.name()))?
            .to_string_lossy()
            // Windows.
            .replace('\\', "/");
        let Some(rel) = strip_top_dir(&path) else {
            continue;
        };

        if !maybe_useful_in_repack(rel) {
            continue;
        }

        let out = dest.join(rel);
        fs::create_dir_all(out.parent().unwrap())?;

        let mut dest = BufWriter::new(File::create(&out)?);
        io::copy(&mut file, &mut dest)?;
        extracted.push(rel.to_owned());
    }
    Ok(extracted)
}

/// Whether a file is kept regardless of whether anything links to it.
fn is_root_file(platform: &Platform, rel: &str) -> bool {
    if rel == platform.gdb_exe()
        || rel.starts_with("arm-none-eabi/share/gdb/")
        || rel.starts_with("distro-info/")
        // Windows: Python's extension modules and its standard library.
        || rel.starts_with("bin/DLLs/")
        || (rel.starts_with("bin/python") && rel.ends_with(".zip"))
    {
        return true;
    }
    // macOS and Linux: lib/python3.X/...
    if let Some(rest) = rel.strip_prefix("lib/python3") {
        let mut parts = rest.split('/').skip(1);
        let first = parts.next().unwrap_or_default();
        // GDB doesn't use these.
        let excluded_module =
            first == "lib-dynload" && parts.next().is_some_and(|m| m.starts_with("_tkinter"));
        return !EXCLUDED_PYTHON_DIRS.contains(&first) && !excluded_module;
    }
    false
}

/// The shared libraries an ELF, Mach-O, or PE file links against. Empty for other files.
fn linked_libraries(path: &Path) -> Result<Vec<String>> {
    let bytes = fs::read(path)?;
    let libs = match Object::parse(&bytes) {
        Ok(Object::Elf(elf)) => elf.libraries,
        Ok(Object::PE(pe)) => pe.libraries,
        // The first entry is "self", the library's own install name.
        Ok(Object::Mach(Mach::Binary(macho))) => macho.libs.into_iter().skip(1).collect(),
        _ => Vec::new(),
    };
    Ok(libs.into_iter().map(str::to_owned).collect())
}

/// Versions of the bundled components in an xPack toolchain.
struct ComponentVersions {
    gdb: String,
    python: String,
}

impl ComponentVersions {
    /// Read the license directory of the given xPack toolchain to infer license info.
    fn detect(work: &Path) -> Result<Self> {
        let names: Vec<String> = fs::read_dir(work.join("distro-info/licenses"))?
            .map(|e| Ok(e?.file_name().to_string_lossy().into_owned()))
            .collect::<io::Result<_>>()?;
        let find = |prefix: &str| {
            names
                .iter()
                .find_map(|n| n.strip_prefix(prefix))
                .map(str::to_owned)
                .with_context(|| format!("no distro-info/licenses/{prefix}* directory"))
        };
        Ok(Self {
            gdb: find("gdb-")?,
            python: find("python-")?,
        })
    }
}

fn release_notes(versions: &ComponentVersions, sums: &str) -> String {
    let ComponentVersions { gdb, python } = versions;
    format!(
        "\
This release contains GDB {gdb} for `arm-none-eabi` with Python {python}, repackaged from
[xPack GNU Arm Embedded GCC {XPACK_VERSION}]({XPACK_URL}/releases/tag/v{XPACK_VERSION}).

```
{sums}```
"
    )
}

struct NewBundle<'a> {
    name: String,
    path: PathBuf,
    src_dir: &'a Path,
    file_paths: &'a BTreeSet<String>,
    readme: &'a str,
}

impl NewBundle<'_> {
    /// Writes a `.tar.gz` with the selected files under a `name/` directory. Symbolic links are
    /// kept, and the timestamps and owners are fixed so the output is reproducible.
    fn write_tar(&self) -> Result<()> {
        let name = &self.name;

        let dest = BufWriter::new(File::create(&self.path)?);
        let gz = GzEncoder::new(dest, Compression::best());
        let mut tar = tar::Builder::new(gz);
        tar.mode(tar::HeaderMode::Deterministic);
        tar.follow_symlinks(false);

        let mut header = tar::Header::new_gnu();
        header.set_size(self.readme.len() as u64);
        header.set_mode(0o644);
        tar.append_data(
            &mut header,
            format!("{name}/README.md"),
            self.readme.as_bytes(),
        )?;

        for rel in self.file_paths {
            tar.append_path_with_name(self.src_dir.join(rel), format!("{name}/{rel}"))?;
        }
        tar.into_inner()?.finish()?.flush()?;
        Ok(())
    }

    /// Writes a `.zip` with the selected files under a `name/` directory.
    fn write_zip(&self) -> Result<()> {
        use zip::{CompressionMethod, DateTime, write::SimpleFileOptions};

        let name = &self.name;
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(9))
            .last_modified_time(DateTime::default());
        let dest = BufWriter::new(File::create(&self.path)?);
        let mut zip = zip::ZipWriter::new(dest);

        zip.start_file(format!("{name}/README.md"), options)?;
        zip.write_all(self.readme.as_bytes())?;
        for rel in self.file_paths {
            zip.start_file(format!("{name}/{rel}"), options)?;
            io::copy(&mut File::open(self.src_dir.join(rel))?, &mut zip)?;
        }
        zip.finish()?.flush()?;
        Ok(())
    }
}
