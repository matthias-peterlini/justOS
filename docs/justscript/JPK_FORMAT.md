# Binary formats

All integers are little-endian.

## `.jpk` package

```
offset  size   field
0       4      magic            4A 50 4B 00  ("JPK\0")
4       2      format version   1
6       2      flags            bit 0 = debug info present
8       4      section count N
12      4      CRC-32 over bytes 4..12 followed by the section table
16      24*N   section table
...            section payloads
```

Section table entry (24 bytes):

```
0   4   tag            "MANI" | "CPOL" | "CODE" | "TYPE" | "ASST"
4   4   flags          bit 0 = DEFLATE-compressed (raw RFC 1951)
8   4   offset         absolute, from start of file
12  4   stored length
16  4   raw length     after decompression (max 256 MiB)
20  4   CRC-32         of the raw payload
```

Unknown section tags are ignored (forward compatibility). Readers reject any
out-of-bounds offset, size mismatch or checksum failure, and bytecode is
verified before it can execute (`Program::verify`).

### `MANI` — manifest
`u32 count`, then `count × (string key, string value)`; strings are
`u32 length + UTF-8`. Keys written by `justc`: `name`, `version`, `entry`,
`profile`, `target`, `compiler`.

### `CPOL` — constant pool
`u32 count`, then per constant a tag byte: `0` = i64, `1` = f64 bits,
`2` = string.

### `CODE` — program
```
u16 entry function index
u16 view function index (0xFFFF = none)
u32 global count, then global names (string)
u32 function count, then per function:
    string name, u8 arity, u16 local slots,
    u32 code length + bytecode,
    u32 line-table length + (u32 offset, u32 line) pairs
```

### `TYPE` — structs and enums
Absent in packages from compilers before 0.3 (and then treated as just the
built-in types). `u32 count`, then per type:

```
string name
u8 kind                      0 = struct, 1 = enum
u32 n, then n × string       struct: field names, in declaration order
   or n × (string, u8)       enum: variant name and number of values
u32 m, then m × (string name, u16 function)   methods taking `self`
```

Types 0 and 1 are always the built-in `Option` (`Some`, `None`) and
`Result` (`Ok`, `Err`).

### `ASST` — assets
`u32 count`, then `count × (string name, u32 length, bytes)`.

## Bytecode

One opcode byte followed by fixed-size operands. Jump targets are absolute
offsets within the function. See `crates/justc-core/src/bytecode.rs` for the
authoritative table; `justc disasm file.jt` prints a listing.

### Data and control instructions added in 0.3

| Instruction | Operands | Effect |
|-------------|----------|--------|
| `Dup` | — | duplicate the top value |
| `Map` | u16 n | pop n key/value pairs into a new map |
| `GetField` / `SetField` | u16 name constant | read / write a struct field by name |
| `NewStruct` | u16 type | pop the fields, in declaration order |
| `NewVariant` | u16 type, u16 variant | pop the variant's values |
| `IsType` | u16 type, u16 variant (`0xFFFF` = any, structs) | pop a value, push whether it matches |
| `GetItem` | u8 position | pop a struct/variant, push its value at that position |
| `InRange` | — | `value lo hi` → `lo <= value < hi` for numbers |
| `Method` | u16 name constant, u8 argc | call the receiver type's method, else the fallback function below the receiver |
| `Closure` | u16 function, u8 n | pop n captured values into a closure |
| `GetCapture` | u8 index | push a value captured by the running closure |
| `Current` | — | push the running function (recursion in nested functions) |
| `Try` | — | unwrap `Some`/`Ok`, or return `None`/`Err` from the function |

A program that uses these runs only on a 0.3 (or newer) runtime; older
runtimes reject it when loading ("invalid opcode").

### Builtins

`Builtin` pushes native function number `u8` from the table
`justc_core::bytecode::BUILTINS`. The table only grows at the end, so
numbers keep their meaning: 0.4 added the standard library (numbers 30 and
up, including the module functions such as `math.sin`). A program calling
a builtin its runtime does not know is rejected when loading ("builtin out
of range").

### UI instructions

An element compiles to:

```
<argument or Nil>  UiBegin kind        open the element (kind: u8, see ui::UiKind)
<value>            UiAttr attr         once per attribute (attr: u8, see ui::Attr)
<callable>         UiOn                event handler, if any
...                                    child elements and attribute lines
                   UiClose             attach the element to its parent
```

Element and attribute numbers are append-only, so a newer runtime always
understands older packages. Packages from the first compiler release use
`UiOpen` / `UiLabel` / `UiButton` (window, column, row, label and button
only); the VM still executes them. An older runtime rejects a newer package
at load time ("invalid opcode") instead of misbehaving.

## Standalone executables

```
[ runtime stub (PE / ELF) ][ .jpk payload ][ trailer (24 bytes) ]

trailer: u64 payload length | u32 payload CRC-32 | u32 version (1) | "JPKBNDL\0"
```

OS loaders ignore appended data (the PE "overlay"), so the stub runs
unmodified, seeks to the trailer at the end of its own file and executes the
payload. For Windows GUI apps the bundler of the original repository patches
the PE optional header's `Subsystem` field (3 → 2) so no console window
appears.

On justOS, `justc build -t windows` makes them: the stub is `tools/just-stub`
(`sys:/system/justc/just-stub.exe`) and the format is in
`system/libs/jpk-bundler`. Its programs keep the console subsystem, since
that stub shows a `view` as text, with its controls numbered.

**Code signing:** sign the *final* `.exe` after bundling (signing the stub and
then appending would invalidate the signature). `signtool` appends its
certificate table after the trailer; the runtime reads the PE security data
directory and finds the trailer right before the certificate table (allowing
for up to 7 bytes of alignment padding), so signed executables keep working.

## Linux `.deb` packages

(The compiler of the original repository; `justc` on justOS does not
make them yet.) `justc build -t deb` wraps the Linux executable above in a Debian package,
written by the bundler itself (`ar` + ustar + gzip, all owned by `root:root`),
so it can be produced on any host:

```
debian-binary     "2.0\n"
control.tar.gz    ./control  (Package, Version, Architecture: amd64, Maintainer,
                              Installed-Size, Depends, Recommends, Section, Description)
data.tar.gz       ./usr/bin/<package>                               0755
                  ./usr/share/applications/<app id>.desktop         apps with a view
                  ./usr/share/icons/hicolor/<n>x<n>/apps/<app id>.png    n = 128, 256, 512, if an icon is set
```

The app id is `dev.justos.<name>` (or `app_id` from `Just.toml`, see
[JUST_DESKTOP.md](JUST_DESKTOP.md)); the `.desktop` entry sets
`StartupWMClass` and `Icon` to it. The manifest of the package records it
as `app_id`, with the display `title`.

The package name is the app name lowercased, with spaces and `_` turned into
`-`. `Depends` comes from the `just-stub.depends` file next to the runtime stub
(generated with `dpkg-shlibdeps` by the release pipeline), defaulting to
`libc6, libgcc-s1`; windowed apps also recommend the X11/Wayland and graphics
libraries that the runtime loads dynamically.
