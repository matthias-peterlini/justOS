#!/usr/bin/env python3
"""Builds a Linux file tree from Debian packages, for justOS's Linux layer.

justOS runs Linux programs on its own kernel; dynamically linked ones need
the dynamic loader (ld-linux-x86-64.so.2), glibc and whatever libraries
they link. This tool takes package names, follows their dependencies in
the Debian archive, downloads the .deb files (checking them against the
archive's SHA-256 sums), and unpacks them into one tree that the image
build copies onto the system volume.

    tools/linux/debroot.py --out build/linux libc6
    tools/linux/debroot.py --out build/linux --dist trixie firefox-esr

Downloads are kept in --cache (build/debcache), so a second run is quick
and offline. The tree follows Debian's merged /usr: /bin, /sbin, /lib and
/lib64 are links into /usr. Documentation, man pages and translations are
left out unless --keep-docs is given, and so are glibc's modules for
legacy character sets. Nothing here runs package scripts: the files are
only unpacked. The caches some of those scripts would make (gdk-pixbuf's
loaders, GLib's compiled settings schemas and modules, the MIME database,
icon theme caches) are made afterwards by running the tree's own tools
through its own dynamic loader, on this machine (Linux x86-64): see
`caches`.

Only the Python standard library is needed. Packages compressed with zstd
(Ubuntu's) need the `zstd` program; Debian's use xz.
"""
import argparse
import hashlib
import io
import lzma
import os
import re
import shutil
import subprocess
import sys
import tarfile
import urllib.request

MIRROR = "http://deb.debian.org/debian"

# Packages that dependencies name but a justOS tree never needs: package
# management, init and login machinery, and interpreters only maintainer
# scripts use.
SKIP = {
    "debconf", "debconf-2.0", "cdebconf", "dpkg", "install-info", "ucf", "perl-base", "perl",
    "init-system-helpers", "adduser", "passwd", "login", "systemd", "systemd-sysv", "sensible-utils",
    "debianutils", "lsb-base", "sysvinit-utils", "usrmerge", "usr-is-merged", "dconf-service",
    "default-dbus-session-bus", "dbus-session-bus", "dbus", "dbus-bin", "dbus-daemon", "dbus-system-bus-common",
    "dbus-session-bus-common", "dbus-user-session", "systemd-timesyncd", "procps", "mount", "util-linux",
    "coreutils", "tar", "gzip", "findutils", "grep", "sed", "bash", "dash", "libc-bin", "tzdata",
    "readline-common", "libpam-runtime", "libpam-modules", "libpam-modules-bin",
}

# Parts of packages left out of the tree (unless --keep-docs): documentation,
# translations, and glibc's iconv modules for legacy character sets (8 MiB;
# UTF-8 needs none of them).
DOCS = ("usr/share/doc/", "usr/share/man/", "usr/share/info/", "usr/share/locale/", "usr/share/lintian/",
        "usr/share/bug/", "usr/share/doc-base/", "usr/share/bash-completion/", "usr/share/menu/",
        "usr/lib/x86_64-linux-gnu/gconv/")


def fetch(url, dest):
    """Downloads `url` to `dest` (through a temporary file)."""
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    tmp = dest + ".part"
    req = urllib.request.Request(url, headers={"User-Agent": "justos-debroot/1.0"})
    with urllib.request.urlopen(req, timeout=120) as r, open(tmp, "wb") as f:
        shutil.copyfileobj(r, f)
    os.replace(tmp, dest)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


# ----------------------------------------------------------------------------
# The archive index


def load_index(mirror, dist, cache, components, refresh):
    """{name: fields} for the newest version of each amd64 package, and
    {virtual name: [providers]}."""
    pkgs, provides = {}, {}
    for comp in components:
        path = os.path.join(cache, f"{dist}-{comp}-Packages.xz")
        if refresh or not os.path.exists(path):
            print(f"debroot: index {dist}/{comp}", file=sys.stderr)
            fetch(f"{mirror}/dists/{dist}/{comp}/binary-amd64/Packages.xz", path)
        text = lzma.open(path, "rt", encoding="utf-8", errors="replace").read()
        for block in text.split("\n\n"):
            fields, key = {}, None
            for line in block.splitlines():
                if line.startswith((" ", "\t")) and key:
                    fields[key] += "\n" + line.strip()
                elif ":" in line:
                    key, _, value = line.partition(":")
                    fields[key] = value.strip()
            name = fields.get("Package")
            if not name:
                continue
            pkgs[name] = fields
            for p in split_deps(fields.get("Provides", "")):
                provides.setdefault(p[0], []).append(name)
    return pkgs, provides


def split_deps(text):
    """A Depends field as a list of alternatives, each a list of names
    (versions and architecture qualifiers dropped)."""
    out = []
    for group in text.split(","):
        alts = []
        for alt in group.split("|"):
            alt = re.sub(r"\(.*?\)|\[.*?\]|<.*?>", "", alt).strip()
            alt = alt.split(":")[0].strip()
            if alt:
                alts.append(alt)
        if alts:
            out.append(alts)
    return out


def resolve(wanted, pkgs, provides, skip):
    """The packages to unpack: `wanted` and everything they depend on."""
    chosen, queue = [], list(wanted)
    seen = set()

    def pick(alts):
        for a in alts:
            if a in seen or a in skip:
                return None  # already there, or deliberately left out
        for a in alts:
            if a in pkgs:
                return a
            for p in provides.get(a, []):
                if p not in skip:
                    return p
        return None

    while queue:
        name = queue.pop(0)
        if name in seen:
            continue
        if name not in pkgs:
            real = [p for p in provides.get(name, []) if p not in skip]
            if not real:
                raise SystemExit(f"debroot: no package {name!r}")
            name = real[0]
            if name in seen:
                continue
        seen.add(name)
        chosen.append(name)
        f = pkgs[name]
        for alts in split_deps(f.get("Pre-Depends", "")) + split_deps(f.get("Depends", "")):
            got = pick(alts)
            if got and got not in seen:
                queue.append(got)
    return chosen


# ----------------------------------------------------------------------------
# Unpacking


def ar_members(data):
    """The members of an `ar` archive (a .deb): {name: bytes}."""
    if data[:8] != b"!<arch>\n":
        raise ValueError("not a .deb")
    out, at = {}, 8
    while at + 60 <= len(data):
        head = data[at:at + 60]
        name = head[:16].decode().strip().rstrip("/")
        size = int(head[48:58].decode().strip())
        at += 60
        out[name] = data[at:at + size]
        at += size + (size & 1)
    return out


def data_tar(deb):
    m = ar_members(open(deb, "rb").read())
    for name, blob in m.items():
        if name.startswith("data.tar"):
            if name.endswith(".xz"):
                return tarfile.open(fileobj=io.BytesIO(lzma.decompress(blob)))
            if name.endswith(".gz") or name == "data.tar":
                return tarfile.open(fileobj=io.BytesIO(blob))
            if name.endswith(".zst"):
                raw = subprocess.run(["zstd", "-dc"], input=blob, capture_output=True, check=True).stdout
                return tarfile.open(fileobj=io.BytesIO(raw))
            raise ValueError(f"{deb}: {name} is in an unknown format")
    raise ValueError(f"{deb}: no data.tar")


def usr_path(name):
    """A package path in the merged-/usr layout (lib/x → usr/lib/x)."""
    name = name.lstrip("./")
    first = name.split("/", 1)[0]
    if first in ("bin", "sbin", "lib", "lib64", "lib32", "libx32"):
        return "usr/" + name
    return name


class Names:
    """JustFS compares names without regard to case, so two paths that
    differ only in case are one place there. Folders take the spelling seen
    first; a file whose name collides with another's is left out."""

    def __init__(self):
        self.seen = {}  # lowercased path -> (spelling, is a folder)
        self.dropped = []

    def place(self, rel, is_dir):
        """The spelling to use for `rel`, or None to leave it out."""
        parts, out = rel.strip("/").split("/"), []
        for i, part in enumerate(parts):
            cand = "/".join(out + [part])
            key = cand.lower()
            last = i == len(parts) - 1
            if key in self.seen:
                spelling, was_dir = self.seen[key]
                if last and not is_dir and spelling != cand:
                    self.dropped.append(rel)
                    return None
                out = spelling.split("/")
            else:
                self.seen[key] = (cand, not last or is_dir)
                out.append(part)
        return "/".join(out)


def unpack(deb, out, keep_docs, names):
    files = 0
    with data_tar(deb) as tar:
        for m in tar.getmembers():
            rel = usr_path(m.name)
            if not rel or rel.startswith("..") or "/../" in rel:
                continue
            if not keep_docs and (rel.rstrip("/") + "/").startswith(DOCS):
                continue
            rel = names.place(rel, m.isdir())
            if rel is None:
                continue
            dest = os.path.join(out, rel)
            if m.isdir():
                os.makedirs(dest, exist_ok=True)
                continue
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            if os.path.lexists(dest):
                os.remove(dest)
            if m.issym():
                os.symlink(m.linkname, dest)
            elif m.islnk():
                src = os.path.join(out, names.place(usr_path(m.linkname), False) or usr_path(m.linkname))
                shutil.copyfile(src, dest)
            elif m.isfile():
                with tar.extractfile(m) as src, open(dest, "wb") as f:
                    shutil.copyfileobj(src, f)
                os.chmod(dest, 0o755 if m.mode & 0o111 else 0o644)
            files += 1
    return files


# The longest name JustFS keeps (bytes); Linux allows 255.
MAX_NAME = 56


def drop_long_names(out):
    """Leaves out what JustFS cannot name (so far only some of the MIME
    database's per-type descriptions, which programs can do without)."""
    gone = []
    for root, dirs, files in os.walk(out, topdown=True):
        for name in list(dirs) + files:
            if len(name.encode()) > MAX_NAME:
                p = os.path.join(root, name)
                if os.path.isdir(p) and not os.path.islink(p):
                    shutil.rmtree(p)
                    dirs.remove(name)
                else:
                    os.remove(p)
                gone.append(os.path.relpath(p, out))
    for rel in gone:
        print(f"debroot: left out {rel} (its name is longer than {MAX_NAME} bytes)", file=sys.stderr)


def run_tool(out, tool, *args, env=None):
    """Runs one of the tree's programs with the tree's loader and libraries;
    its output, or None if it cannot run here."""
    loader = os.path.join(out, "usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2")
    libs = os.path.join(out, "usr/lib/x86_64-linux-gnu")
    if not os.path.exists(loader) or not os.path.exists(os.path.join(out, tool)):
        return None
    try:
        r = subprocess.run([loader, "--library-path", libs, os.path.join(out, tool), *args],
                           capture_output=True, env={**os.environ, **(env or {})}, timeout=300)
    except OSError:
        return None
    if r.returncode != 0:
        print(f"debroot: {tool}: {r.stderr.decode(errors='replace').strip()[:200]}", file=sys.stderr)
        return None
    return r.stdout


def caches(out):
    """What package scripts would have made, for the programs that need it:
    each step runs only when its tool is in the tree."""
    made = []
    lib = "usr/lib/x86_64-linux-gnu"
    # gdk-pixbuf: which loader module reads which picture format. The
    # paths in the cache are the tree's, as programs see them.
    mods = os.path.join(out, lib, "gdk-pixbuf-2.0/2.10.0/loaders")
    if os.path.isdir(mods):
        files = sorted(os.path.join(mods, f) for f in os.listdir(mods) if f.endswith(".so"))
        text = run_tool(out, f"{lib}/gdk-pixbuf-2.0/gdk-pixbuf-query-loaders", *files)
        if text is not None:
            text = text.decode().replace(os.path.abspath(out) + "/", "/").replace(out.rstrip("/") + "/", "/")
            with open(os.path.join(mods, "..", "loaders.cache"), "w") as f:
                f.write(text)
            made.append("gdk-pixbuf loaders")
    # GLib: settings schemas, compiled; GIO's modules.
    schemas = os.path.join(out, "usr/share/glib-2.0/schemas")
    if os.path.isdir(schemas) and run_tool(out, f"{lib}/glib-2.0/glib-compile-schemas", schemas) is not None:
        made.append("GSettings schemas")
    gio = os.path.join(out, lib, "gio/modules")
    if os.path.isdir(gio) and run_tool(out, f"{lib}/glib-2.0/gio-querymodules", gio) is not None:
        made.append("GIO modules")
    # The MIME database (types, globs, magic).
    mime = os.path.join(out, "usr/share/mime")
    if os.path.isdir(os.path.join(mime, "packages")) and run_tool(out, "usr/bin/update-mime-database", mime) is not None:
        made.append("MIME database")
    # Icon themes: an index of every icon, so a program need not look
    # through the folders.
    icons = os.path.join(out, "usr/share/icons")
    if os.path.isdir(icons):
        n = 0
        for theme in sorted(os.listdir(icons)):
            d = os.path.join(icons, theme)
            if os.path.exists(os.path.join(d, "index.theme")):
                if run_tool(out, "usr/bin/gtk-update-icon-cache", "-q", "-f", "-t", d) is not None:
                    n += 1
        if n:
            made.append(f"{n} icon caches")
    if made:
        print("debroot: made " + ", ".join(made))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("packages", nargs="+")
    ap.add_argument("--out", required=True, help="the tree to build (replaced)")
    ap.add_argument("--dist", default="trixie")
    ap.add_argument("--mirror", default=MIRROR)
    ap.add_argument("--cache", default="build/debcache")
    ap.add_argument("--components", default="main")
    ap.add_argument("--skip", default="", help="more packages to leave out, comma-separated")
    ap.add_argument("--refresh", action="store_true", help="download the index again")
    ap.add_argument("--keep-docs", action="store_true")
    ap.add_argument("--list", action="store_true", help="only print the packages")
    a = ap.parse_args()

    skip = SKIP | {s for s in a.skip.split(",") if s}
    pkgs, provides = load_index(a.mirror, a.dist, a.cache, a.components.split(","), a.refresh)
    chosen = resolve(a.packages, pkgs, provides, skip - set(a.packages))
    if a.list:
        for n in chosen:
            print(n, pkgs[n]["Version"], pkgs[n].get("Installed-Size", "?"), "KiB")
        return

    if os.path.isdir(a.out):
        shutil.rmtree(a.out)
    os.makedirs(os.path.join(a.out, "usr"))
    total = 0
    names = Names()
    for n in chosen:
        f = pkgs[n]
        deb = os.path.join(a.cache, "pool", os.path.basename(f["Filename"]))
        if not os.path.exists(deb) or sha256(deb) != f["SHA256"]:
            print(f"debroot: download {n} {f['Version']}", file=sys.stderr)
            fetch(f"{a.mirror}/{f['Filename']}", deb)
            if sha256(deb) != f["SHA256"]:
                os.remove(deb)
                raise SystemExit(f"debroot: {n}: checksum mismatch")
        total += unpack(deb, a.out, a.keep_docs, names)
    for d in ("bin", "sbin", "lib", "lib64"):
        os.makedirs(os.path.join(a.out, "usr", d), exist_ok=True)
        os.symlink(f"usr/{d}", os.path.join(a.out, d))
    caches(a.out)
    drop_long_names(a.out)
    with open(os.path.join(a.out, "usr", "PACKAGES"), "w") as f:
        for n in chosen:
            f.write(f"{n} {pkgs[n]['Version']}\n")
    for rel in names.dropped:
        print(f"debroot: left out {rel} (its name differs only in case from another)", file=sys.stderr)
    print(f"debroot: {len(chosen)} packages, {total} files in {a.out}")


if __name__ == "__main__":
    main()
