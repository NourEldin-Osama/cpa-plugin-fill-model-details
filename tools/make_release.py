"""Build the release zip of the plugin.

Usage:
  uv run --no-project python -I tools/make_release.py

Steps:
1. Build the DLL in target/dist/build. Folder names of this machine are
   replaced by short names, and the build time is left out. So the file does
   not show where it was built, and the same source gives the same file.
2. Check the DLL: no folder name of this machine inside, and no need for the
   Microsoft Visual C++ runtime.
3. Collect the licence texts of the Rust libraries that are built into the DLL.
4. Write target/dist/fill-model-details-v<version>-windows-amd64.zip.
"""

import hashlib
import json
import pathlib
import subprocess
import sys
import zipfile

TARGET = "x86_64-pc-windows-msvc"
ZIP_PLATFORM = "windows-amd64"
INSTALLED_DLL_NAME = "fill-model-details.dll"
NOTICES_FOLDER = "third-party-licenses"
LICENCE_NAME_STARTS = ("LICENSE", "LICENCE", "COPYING", "UNLICENSE", "NOTICE")
# The notices of the Rust standard library, inside the Rust toolchain folder.
RUST_NOTICES = pathlib.PurePath("share", "doc", "rust", "COPYRIGHT-library.html")

NOTICE_HEADER = """\
Third-party licences

fill-model-details.dll contains code from the Rust libraries listed below.
Where a library gives a choice of licences, every licence text of that
library is included.

The DLL also contains the Rust standard library (MIT OR Apache-2.0). Its
notices are in rust-standard-library.html in this folder. The licence of the
page's icons is in lucide.txt.

Libraries
---------
"""


def fail(message):
    sys.exit(f"make_release: {message}")


def run(command, folder):
    result = subprocess.run(command, cwd=folder, capture_output=True, text=True, encoding="utf-8")
    if result.returncode != 0:
        fail(f"{' '.join(command[:2])} failed:\n{result.stderr.strip()}")
    return result.stdout


def read_metadata(repo):
    command = ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", TARGET]
    return json.loads(run(command, repo))


def linked_packages(metadata):
    """The plugin's own package, and every library it reaches through normal dependencies."""
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    packages = {package["id"]: package for package in metadata["packages"]}
    root_id = metadata["resolve"]["root"]
    found = {root_id}
    queue = [root_id]
    while queue:
        package_id = queue.pop()
        for dependency in nodes[package_id]["deps"]:
            is_normal = any(kind["kind"] is None for kind in dependency["dep_kinds"])
            if is_normal and dependency["pkg"] not in found:
                found.add(dependency["pkg"])
                queue.append(dependency["pkg"])
    found.remove(root_id)
    libraries = sorted((packages[package_id] for package_id in found), key=lambda package: package["name"])
    return packages[root_id], libraries


def source_folders(libraries):
    """The folders that hold the libraries' source code, for example the cargo registry."""
    return sorted({pathlib.Path(library["manifest_path"]).parent.parent for library in libraries})


def build(repo, build_folder, sysroot, library_folders):
    """Build the DLL with this machine's folders replaced by /rust and /deps."""
    flags = [f"--remap-path-prefix={sysroot}=/rust"]
    flags += [f"--remap-path-prefix={folder}=/deps" for folder in library_folders]
    # /Brepro stops the linker from writing the build time into the file, so
    # the same source gives the same DLL each time, byte for byte.
    flags += ["-C", "link-arg=/Brepro"]
    # A JSON list of strings is also a valid TOML list. Cargo adds these flags
    # to the ones in .cargo/config.toml.
    rustflags = json.dumps(flags, ensure_ascii=False)
    command = [
        "cargo", "build", "--release", "--locked",
        "--target", TARGET,
        "--target-dir", str(build_folder),
        "--config", f"target.{TARGET}.rustflags={rustflags}",
    ]
    if subprocess.run(command, cwd=repo).returncode != 0:
        fail("cargo build failed")


def check_dll(dll_bytes, machine_folders):
    lowered = dll_bytes.lower()
    for folder in machine_folders:
        for text in {str(folder), str(folder).replace("\\", "/")}:
            for encoded in (text.encode("utf-8"), text.encode("utf-16-le")):
                if encoded.lower() in lowered:
                    fail(f"the DLL still holds the folder name {text}")
    if b"vcruntime" in lowered or b"msvcp" in lowered:
        fail("the DLL needs the Microsoft Visual C++ runtime; see .cargo/config.toml")


def licence_files(library):
    folder = pathlib.Path(library["manifest_path"]).parent
    files = sorted(
        path for path in folder.iterdir()
        if path.is_file() and path.name.upper().startswith(LICENCE_NAME_STARTS)
    )
    if not files:
        fail(f"no licence file found for {library['name']} {library['version']} in {folder}")
    return files


def libraries_notice(libraries):
    """The list of libraries, then every different licence text once."""
    users_of_text = {}
    lines = [NOTICE_HEADER.rstrip("\n")]
    for library in libraries:
        label = f"{library['name']} {library['version']}"
        licence = library.get("license") or "see its licence text"
        lines.append(f"- {label}: {licence}, {library.get('repository') or 'no repository given'}")
        for path in licence_files(library):
            text = path.read_text(encoding="utf-8").replace("\r\n", "\n").strip()
            users_of_text.setdefault(text, []).append(f"{label} ({path.name})")
    for text, users in users_of_text.items():
        lines += ["", "=" * 78, "Used by: " + ", ".join(users), "=" * 78, "", text]
    return "\n".join(lines) + "\n"


def text_with_lf(path):
    """The file's bytes with LF line ends, whatever Git wrote into the working folder."""
    return path.read_bytes().replace(b"\r\n", b"\n")


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def main():
    repo = pathlib.Path(__file__).resolve().parent.parent
    metadata = read_metadata(repo)
    plugin, libraries = linked_packages(metadata)
    library_folders = source_folders(libraries)
    sysroot = pathlib.Path(run(["rustc", "--print", "sysroot"], repo).strip())

    rust_notices = sysroot / RUST_NOTICES
    icon_licence = repo / NOTICES_FOLDER / "lucide.txt"
    for needed in (rust_notices, icon_licence):
        if not needed.is_file():
            fail(f"missing licence file {needed}")
    notice = libraries_notice(libraries)

    dist_folder = pathlib.Path(metadata["target_directory"]) / "dist"
    build_folder = dist_folder / "build"
    build(repo, build_folder, sysroot, library_folders)

    library_name = next(target["name"] for target in plugin["targets"] if "cdylib" in target["kind"])
    dll_bytes = (build_folder / TARGET / "release" / f"{library_name}.dll").read_bytes()
    check_dll(dll_bytes, [sysroot, pathlib.Path.home(), repo, *library_folders])

    zip_path = dist_folder / f"{plugin['name']}-v{plugin['version']}-{ZIP_PLATFORM}.zip"
    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr(INSTALLED_DLL_NAME, dll_bytes)
        archive.writestr("README.md", text_with_lf(repo / "README.md"))
        archive.writestr("LICENSE", text_with_lf(repo / "LICENSE"))
        archive.writestr(f"{NOTICES_FOLDER}/rust-libraries.txt", notice)
        archive.write(rust_notices, f"{NOTICES_FOLDER}/rust-standard-library.html")
        archive.writestr(f"{NOTICES_FOLDER}/lucide.txt", text_with_lf(icon_licence))

    print(f"{INSTALLED_DLL_NAME}: {len(dll_bytes)} bytes, SHA-256 {sha256(dll_bytes)}")
    print(f"{zip_path}: {zip_path.stat().st_size} bytes, SHA-256 {sha256(zip_path.read_bytes())}")


if __name__ == "__main__":
    main()
