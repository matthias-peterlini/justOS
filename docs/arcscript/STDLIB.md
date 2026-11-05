# ArcScript — Standard Library

Everything a program needs beyond the core language: text, lists, math,
random numbers, dates, files, JSON, the command line, other programs and
HTTP. It is part of the runtime (no packages to install) and works the same
on Windows and Linux.

* **Global functions** — strings, lists, input and conversions — need no
  import. Like every function they can also be called as methods, with the
  first argument in front: `split(line, ",")` is `line.split(",")`.
* **Modules** — `math`, `random`, `time`, `fs`, `path`, `json`, `os` and
  `http` — are imported first, then used qualified, or imported by name:

```arcscript
import math                      # math.sqrt(2), math.pi
import json.{parse, stringify}   # parse(text), stringify(value)
```

  These names are reserved: a file `math.as` next to a program is not
  loaded by `import math`.

* **Errors are values.** Operations that can fail for reasons outside the
  program — a missing file, invalid JSON, a network error — return a
  `Result`: `Ok(value)` or `Err(message)`. Handle it with `match`, `?`,
  `unwrap_or` or `is_ok` (see [Option, Result and errors](LANGUAGE.md#option-result-and-errors)).
  Calling a function with the wrong kind of value (`upper(3)`) is a bug and
  stops the program with a runtime error.

The rest of the builtins (`print`, `len`, `str`, `push`, `get`, `unwrap`,
...) are listed in [LANGUAGE.md](LANGUAGE.md#builtins).

A complete program using most of this page is
[`examples/notes.as`](../examples/notes.as), a command-line notebook saved
as JSON.

## Strings

Positions and lengths count characters (`"è"` is one), not bytes.

| Function | Description |
|---|---|
| `split(text, sep?)` | list of the parts between `sep`; without `sep`, the words |
| `trim(text)` | without leading and trailing whitespace |
| `trim_start(text)` | without leading whitespace |
| `trim_end(text)` | without trailing whitespace |
| `upper(text)` | in upper case |
| `lower(text)` | in lower case |
| `replace(text, from, to)` | every `from` replaced by `to` |
| `starts_with(text, prefix)` | whether text begins with prefix |
| `ends_with(text, suffix)` | whether text ends with suffix |
| `index_of(text or list, needle)` | `Some(first position)` or None |
| `chars(text)` | list of the characters |
| `repeat(text or list, n)` | n copies joined together |
| `slice(text or list, start, end?)` | the part from start to end (exclusive); negative counts from the end |
| `lines(text)` | list of the lines |
| `pad_start(text, width, fill?)` | padded on the left to width |
| `pad_end(text, width, fill?)` | padded on the right to width |
| `ord(char)` | the Unicode code of a one-character string |
| `chr(code)` | the one-character string for a Unicode code |

```arcscript
let line = "  Ada Lovelace, 1815 "
let parts = line.trim().split(", ")          # ["Ada Lovelace", "1815"]
print(parts[0].upper(), parts[0].index_of("Love"))   # ADA LOVELACE Some(4)
print("7".pad_start(3, "0"), "abc".slice(-2), "a-b-c".replace("-", "+"))  # 007 bc a+b+c
```

## Lists

Functions that take a function `f` accept a lambda (`|x| x * 2`), a named
function or a method value. `sort`, `reverse`, `extend` and `clear` change
the list; the others return a new one. Values are ordered as with `<`:
numbers, strings, bools, and lists element by element; mixing numbers and
strings is an error.

| Function | Description |
|---|---|
| `map(list, f)` | new list of f(item) for each item |
| `filter(list, f)` | new list of the items where f(item) is true |
| `reduce(list, initial, f)` | fold: acc = f(acc, item) for each item |
| `sort(list, key?)` | sort in place, by key(item) when given |
| `sorted(list, key?)` | sorted copy |
| `reverse(list)` | reverse in place |
| `reversed(list or text)` | reversed copy |
| `sum(list)` | sum of the numbers |
| `any(list, f?)` | whether some item (or f(item)) is true |
| `all(list, f?)` | whether every item (or f(item)) is true |
| `first(list, f?)` | `Some(first item, or first where f(item)` is true) or None |
| `last(list)` | `Some(last item)` or None |
| `min(a, b) or min(list)` | smaller value |
| `max(a, b) or max(list)` | larger value |
| `range(start, end, step?)` | list of ints from start up to end (exclusive) |
| `zip(a, b)` | list of [a[i], b[i]] pairs |
| `items(map)` | list of [key, value] pairs, in key order |
| `extend(list, other)` | append every item of other |
| `clear(list or map)` | remove every item |

```arcscript
let scores = [72, 95, 88, 61]
print(scores.filter(|s| s >= 70).map(|s| s / 10))   # [7, 9, 8]
print(scores.sum(), max(scores), scores.sorted())     # 316 95 [61, 72, 88, 95]
let people = [["Ada", 36], ["Alan", 41], ["Grace", 85]]
people.sort(|p| -p[1])                                # oldest first
print(people.first(|p| p[1] > 40))                    # Some(["Grace", 85])
```

## Input and conversions

| Function | Description |
|---|---|
| `read_line(prompt?)` | `Some(line typed on standard input)` or None at end of input |
| `parse_int(text)` | `Some(int)` or None |
| `parse_float(text)` | `Some(float)` or None |
| `ok_or(option, error)` | `Ok(value)` for `Some(value)`, `Err(error)` for None |

`read_line` returns `None` when the input ends, and always in programs
started without a console (the ArcStudio preview, windowed apps).
`int(text)` stops the program on invalid text; `parse_int` lets you decide:

```arcscript
let answer = read_line("How old are you? ").unwrap_or("")
match parse_int(answer)
    Some(age) => print("In ten years you will be {age + 10}")
    None => print("`{answer}` is not a number")
```

## math

`math.pi`, `math.tau`, `math.e`, `math.inf` and `math.nan` are constants.
Angles are in radians. `floor`, `ceil`, `trunc` and `round` return ints.

| Function | Description |
|---|---|
| `math.sin(x)` | sine (radians) |
| `math.cos(x)` | cosine (radians) |
| `math.tan(x)` | tangent (radians) |
| `math.asin(x)` | arc sine |
| `math.acos(x)` | arc cosine |
| `math.atan(x)` | arc tangent |
| `math.atan2(y, x)` | angle of the point (x, y) |
| `math.exp(x)` | e to the power x |
| `math.log(x, base?)` | logarithm, natural by default |
| `math.log10(x)` | base-10 logarithm |
| `math.log2(x)` | base-2 logarithm |
| `math.pow(x, y)` | x to the power y (int when both are ints and y >= 0) |
| `math.floor(x)` | largest int <= x |
| `math.ceil(x)` | smallest int >= x |
| `math.round(x, digits?)` | nearest int, or float rounded to digits decimals |
| `math.trunc(x)` | x without its fractional part, as int |
| `math.sign(x)` | -1, 0 or 1 |
| `math.clamp(x, lo, hi)` | x limited to lo..=hi |
| `math.hypot(x, y)` | length of the vector (x, y) |
| `math.is_nan(x)` | whether x is NaN |
| `math.is_finite(x)` | whether x is neither infinite nor NaN |
| `math.sqrt(x)` | square root |
| `math.abs(x)` | absolute value |
| `math.min(a, b) or math.min(list)` | smaller value |
| `math.max(a, b) or math.max(list)` | larger value |

## random

Pseudo-random numbers, different on every run unless `random.seed(n)` is
called. Not suitable for passwords or cryptography.

| Function | Description |
|---|---|
| `random.seed(n)` | make the following numbers repeatable |
| `random.int(lo, hi)` | random int with lo <= n < hi |
| `random.float()` | random float in [0, 1) |
| `random.choice(list)` | a random item |
| `random.shuffle(list)` | shuffle in place |

```arcscript
import random
let dice = random.int(1, 7)              # 1 to 6
let card = random.choice(["♠", "♥", "♦", "♣"])
```

## time

Timestamps are seconds since 1970-01-01 00:00 UTC. Dates are in UTC (time
zones are not supported yet). `time.date` returns a map with `year`,
`month`, `day`, `hour`, `minute`, `second`, `weekday` (1 = Monday … 7 =
Sunday) and `yearday` (1 = January 1st).

| Function | Description |
|---|---|
| `time.now()` | seconds since 1970-01-01 UTC, as float |
| `time.sleep(seconds)` | pause the program |
| `time.date(timestamp?)` | map of year, month, day, hour, minute, second, weekday (UTC) |
| `time.format(timestamp, pattern?)` | text such as "2026-10-21 09:30:00" (%Y %m %d %H %M %S %j %a %b, UTC) |
| `time.timestamp(year, month, day, hour?, minute?, second?)` | seconds since 1970 (UTC) |

`time.format` patterns: `%Y` year, `%m` month, `%d` day, `%H` hour, `%M`
minute, `%S` second (all zero-padded), `%j` day of the year, `%a` weekday
(`Mon`), `%b` month (`Oct`), `%%` a percent sign. The default is
`"%Y-%m-%d %H:%M:%S"`.

```arcscript
import time
let start = time.now()
let release = time.timestamp(2026, 12, 24)
print("days to go:", (release - int(start)) / 86400)
print(time.format(start, "%d/%m/%Y"), time.date(start)["weekday"])
```

## fs

Paths are relative to the current directory (`os.cwd()`) unless absolute.
Text files are read and written as UTF-8.

| Function | Description |
|---|---|
| `fs.read(path)` | `Ok(text)` or `Err(message)` |
| `fs.write(path, text)` | replace the file's content; `Ok(nil)` or `Err(message)` |
| `fs.append(path, text)` | add to the end of the file; `Ok(nil)` or `Err(message)` |
| `fs.exists(path)` | whether a file or directory exists |
| `fs.is_file(path)` | whether path is a file |
| `fs.is_dir(path)` | whether path is a directory |
| `fs.list(dir)` | `Ok(sorted list of entry names)` or `Err(message)` |
| `fs.mkdir(path)` | create a directory and its parents; `Ok(nil)` or `Err(message)` |
| `fs.remove(path)` | delete a file or an empty directory; `Ok(nil)` or `Err(message)` |
| `fs.rename(from, to)` | move or rename; `Ok(nil)` or `Err(message)` |
| `fs.copy(from, to)` | copy a file; `Ok(nil)` or `Err(message)` |

```arcscript
import fs
match fs.read("settings.txt")
    Ok(text) => print(len(text.lines()), "lines")
    Err(message) => print("cannot read the settings:", message)
fs.mkdir("out").unwrap()
fs.write("out/report.txt", "done\n").unwrap()
```

## path

Text operations on paths; nothing is read from disk.

| Function | Description |
|---|---|
| `path.join(parts...)` | parts joined with the system separator |
| `path.name(path)` | the last component ("a/b.txt" → "b.txt") |
| `path.stem(path)` | the file name without extension |
| `path.ext(path)` | the extension without the dot, or "" |
| `path.parent(path)` | the directory containing path |

## json

| Function | Description |
|---|---|
| `json.parse(text)` | `Ok(value)` or `Err(message)`; objects become maps, null becomes nil |
| `json.stringify(value, indent?)` | JSON text; structs become objects |

| JSON | ArcScript |
|---|---|
| object | map (keys sorted) |
| array | list |
| string, `true`/`false` | string, bool |
| number | int if it has no fraction or exponent and fits, else float |
| `null` | `nil` |

`json.stringify` also accepts structs (objects with their fields in
declaration order), ranges (arrays), `Some(x)` (as `x`) and `None` (as
`null`); other enum values become `"Variant"` or `{"Variant": value}`.
`NaN` and infinities become `null`; functions are an error. Remember that
`{` starts an interpolation in string literals: write `\{` in JSON text
typed in the source, or read it from a file.

```arcscript
import fs
import json
struct Settings
    theme = "dark"
    size = 14
fs.write("settings.json", json.stringify(Settings(), 2))
let data = json.parse(fs.read("settings.json").unwrap()).unwrap()
print(data["theme"], data["size"] + 1)
```

## os

`os.name` (`"windows"`, `"linux"` or `"macos"`) and `os.sep` (the path
separator) are values, not functions.

| Function | Description |
|---|---|
| `os.args()` | list of the command-line arguments |
| `os.env(name)` | `Some(value of the environment variable)` or None |
| `os.cwd()` | the current directory |
| `os.exit(code?)` | stop the program with an exit code (default 0) |
| `os.run(program, args?)` | run a program; `Ok({code, stdout, stderr})` or `Err(message)` |
| `os.notify(title, body?)` | show a desktop notification; `Ok(id)` or `Err(message)` |
| `os.name` | the operating system: "windows", "linux" or "macos" |
| `os.sep` | the path separator: "\\" on Windows, "/" elsewhere |

`os.args()` holds the arguments after the program: `arcc run app.as -- a b`,
`arc-runner app.arc a b` or `app.exe a b` all give `["a", "b"]`.
`os.exit(code)` ends the program at once; in a windowed app it closes the
window. `os.run` waits for the program to finish and captures its output;
the arguments are passed as they are, without a shell (so `*` or `|` have
no special meaning):

```arcscript
import os
match os.run("git", ["log", "-1", "--format=%s"])
    Ok(result) if result["code"] == 0 => print("last commit:", result["stdout"].trim())
    Ok(result) => print("git failed:", result["stderr"])
    Err(message) => print("cannot start git:", message)
```

`os.notify` uses the freedesktop notification service on the D-Bus session
bus (Linux). The notification carries the app's title as its sender and the
app id (`dev.justos.<name>`, see [ARC_DESKTOP.md](ARC_DESKTOP.md)) as its
icon and `desktop-entry`, so the notification center groups it with the
app. Elsewhere, or without a notification service, it returns `Err`:

```arcscript
import os
if os.notify("Export finished", "3 files saved").is_err()
    print("export finished")
```

## http

Blocking HTTP and HTTPS requests (TLS certificates are checked against the
built-in Mozilla root store). The response is a map with `status` (int),
`body` (string) and `headers` (a map with lower-case names). Any response is
`Ok`, including `404` or `500`: check `status`. `Err` means that no
response arrived (no network, unknown host, timeout after 60 s). The
proxies in `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` are used, except for
hosts in `NO_PROXY` and for `localhost`.

| Function | Description |
|---|---|
| `http.get(url, headers?)` | `Ok({status, body, headers})` or `Err(message)` |
| `http.post(url, body, headers?)` | `Ok({status, body, headers})` or `Err(message)`; a map or list body is sent as JSON |
| `http.request(method, url, body?, headers?)` | `Ok({status, body, headers})` or `Err(message)` |

```arcscript
import http
import json

fn latest_release(repo)
    let url = "https://api.github.com/repos/{repo}/releases/latest"
    let response = http.get(url, {"Accept": "application/vnd.github+json"})?
    if response["status"] != 200
        return Err("GitHub answered {response["status"]}")
    return Ok(json.parse(response["body"])?["tag_name"])

print(latest_release("rust-lang/rust"))
```

Runtimes built without the `net` feature return
`Err("this runtime was built without network support")`.
