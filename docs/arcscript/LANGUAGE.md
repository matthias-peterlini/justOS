# ArcScript — Language Reference

ArcScript is a small, strict, indentation-based language for JustOS applications.
Source files use the `.as` extension (the legacy `.arcs` is still accepted)
and compile to ArcVM bytecode.

## Lexical structure

| Element      | Syntax                                                                 |
|--------------|------------------------------------------------------------------------|
| Comment      | `# until end of line`                                                  |
| Integer      | `42`, `1_000`, `0xff`, `0b1010`, `0o17` (64-bit signed)                 |
| Float        | `3.14`, `1e-3`, `2.5E10`                                               |
| String       | `"text"`, escapes `\n \t \r \0 \\ \" \{ \} \u{1F600}`                   |
| Interpolation| `"Hello {name}, {a + b}"` — any expression inside `{ }`                |
| Booleans/nil | `true`, `false`, `nil`                                                 |

### Indentation

Blocks are delimited by indentation, **no `:` required** (a trailing `:` is
accepted for readability). The lexer emits `INDENT`/`DEDENT` tokens:

* a deeper indentation opens a block, returning to an outer level closes it;
* dedenting to a level that was never opened is an error;
* blank and comment-only lines are ignored;
* newlines inside `( )` and `[ ]` continue the logical line;
* a tab counts as 4 columns (spaces are recommended).

## Statements

```arcscript
let x = 10              # declare (top level: global, otherwise block-local)
x = x + 1               # assign
x += 2                  # also -=, *=, /=
items[0] = "first"      # indexed assignment

if x > 10
    print("big")
elif x > 5              # `else if` also works
    print("medium")
else
    print("small")

while x > 0
    x -= 1

for i in 0..10          # half-open range
    if i == 3
        continue
    if i == 8
        break

for item in [1, 2, 3]   # lists, strings (by character) and ranges are iterable
    print(item)

fn area(w, h)           # top-level functions can be called before their declaration
    return w * h
```

Keywords: `let fn return if elif else while for in break continue true false
nil and or not view struct enum match import`. UI element names (`label`,
`button`, ...) are contextual and remain usable as variable names.

## Expressions & operators

Precedence from lowest to highest:

| Operators                  | Notes                                       |
|----------------------------|---------------------------------------------|
| `or`                       | short-circuit, operands must be `bool`      |
| `and`                      | short-circuit, operands must be `bool`      |
| `not`                      | operand must be `bool`                      |
| `== != < <= > >=`          | comparisons cannot be chained               |
| `..`                       | integer range                               |
| `+ -`                      | `+` also concatenates strings and lists     |
| `* / %`                    | `int / int` is integer division; `"ab" * 3` |
| unary `-`                  |                                             |
| `f(x)` `a[i]` `a.x` `a.f(x)` `a?` | see [Structs](#structs) and [Errors](#option-result-and-errors) |

A lambda `|a, b| expr` extends as far to the right as possible.

ArcScript is **strict**: conditions must be `bool` (no truthiness), integer
overflow and division by zero are runtime errors, and indices are
bounds-checked (negative indices count from the end).

## Types

`nil`, `bool`, `int` (i64), `float` (f64), `string` (immutable, UTF-8),
`list`, `map`, `range`, `function`, and the structs and enums a program
declares (plus the built-in enums `Option` and `Result`).

Lists, maps and structs are mutable and shared **by reference**: after
`let b = a`, changing `b` changes `a`. Use `copy(a)` for a separate
(shallow) copy. Strings, numbers and enum values are immutable.

## Maps

```arcscript
let ages = {"ada": 36, "linus": 54}
let empty = {}
ages["grace"] = 85              # insert or replace
ages["ada"] += 1
print(ages["ada"])              # a missing key is a runtime error
print(ages.get("bob"))          # None — or Some(value)
for name in ages                # keys, in sorted order
    print(name, ages[name])
print(len(ages), contains(ages, "ada"), keys(ages), values(ages))
remove(ages, "linus")           # Some(54)
```

Keys are strings, ints or bools; iteration is always in sorted key order,
so output is deterministic. A map literal may span several lines.

## Structs

```arcscript
struct Point
    x
    y = 0                       # a default value

    fn length(self)             # a method: first parameter `self`
        return sqrt(float(self.x * self.x + self.y * self.y))

    fn moved(self, dx, dy)
        return Point(self.x + dx, self.y + dy)

    fn origin()                 # an associated function (no `self`)
        return Point(0, 0)

let p = Point(x: 3, y: 4)       # by name, in any order
let q = Point(3)                # by position; y uses its default
p.x = 10
p.y += 1
print(p.length(), p.moved(1, 1), Point.origin())
print(p)                        # Point(x: 10, y: 5)
```

* Fields are listed first (one per line, or comma separated), then methods.
* Missing fields without a default, unknown fields and misspelled names are
  compile-time errors with suggestions.
* `value.method(args)` calls the method of the value's type. When the type
  has no such method, it calls the function `method(value, args)`, so
  `items.push(4)` and `"hi".len()` keep working (uniform call syntax).
* Defaults can use globals and functions, not the caller's local variables.

## Enums and `match`

```arcscript
enum Shape
    Circle(radius)              # a variant with a value
    Rect(w, h)
    Empty                       # a variant without values

    fn area(self)
        match self
            Circle(r) => return 3.14159 * r * r
            Rect(w, h) => return w * h
            Empty => return 0

let shapes = [Shape.Circle(2.0), Shape.Rect(3, 4), Shape.Empty]

for shape in shapes
    match shape
        Rect(w, h) if w == h => print("a square of side {w}")   # guard
        Rect(w, h) => print("a rectangle")
        Circle(_) | Empty                                         # alternatives
            print("something round, or nothing")
    print("area:", shape.area())
```

An arm is `pattern [if guard] => statement` or `pattern [if guard]` followed
by an indented block. The first matching arm runs. Patterns:

| Pattern | Matches |
|---------|---------|
| `_` | anything |
| `name` | anything, binding it to `name` |
| `0`, `-1`, `2.5`, `"text"`, `true`, `nil` | an equal value |
| `1..10` | a number with `1 <= x < 10` |
| `Circle(r)`, `Shape.Circle(r)`, `Some(x)` | an enum variant, matching its values against sub-patterns |
| `Point(x, 0)` | a struct, matching its fields in declaration order |
| `a \| b \| c` | any of the alternatives (which cannot bind names) |

Patterns nest: `Some(Rect(w, _))`. Variants can be written without their
enum name when only one enum has that variant.

A `match` over the variants of an enum must cover all of them (or end with
`_`): a forgotten case is a compile-time error. Other matches without a
catch-all arm get a warning, and a value that matches no arm stops the
program with an error.

`if let` tests a single pattern:

```arcscript
if let Some(age) = ages.get("ada")
    print("ada is {age}")
elif let Err(e) = parse(text)
    print("error: {e}")
else
    print("nothing")
```

## Option, Result and errors

Two enums are built in:

* `Option`: `Some(value)` or `None` — a value that may be missing.
* `Result`: `Ok(value)` or `Err(error)` — the outcome of something that can fail.

```arcscript
fn parse_age(text)
    let n = int(text)
    if n < 0
        return Err("negative age: {n}")
    return Ok(n)

fn total(a, b)
    let x = parse_age(a)?       # on Err, return that Err from `total`
    let y = parse_age(b)?
    return Ok(x + y)

match total("3", "-1")
    Ok(sum) => print("sum: {sum}")
    Err(message) => print("error: {message}")
```

`expr?` unwraps `Some`/`Ok` and otherwise returns the `None`/`Err` from the
enclosing function, like Rust's `?`. It is not allowed in top-level code or
in the `view`; use `match`, `unwrap_or` or `if let` there.

Helpers (also callable as methods, e.g. `opt.unwrap_or(0)`): `unwrap`,
`expect(x, message)`, `unwrap_or(x, default)`, `ok_or(option, error)`
(turns `None` into `Err(error)`, handy before `?`), `is_some`, `is_none`,
`is_ok`, `is_err`. `panic(message)` stops the program with an error, and
`assert(condition, message)` panics when the condition is false.

## Closures

Functions can be declared inside functions, and `|params| expression`
creates a lambda. Both are closures: they capture the variables they use
from the enclosing function **by value**, when they are created.

```arcscript
fn make_counter(start)
    let state = [start]          # shared, mutable state lives in a list...
    fn next()
        state[0] += 1            # ...so the closure can update it
        return state[0]
    return next

let counter = make_counter(10)
print(counter(), counter())      # 11 12

fn apply(items, f)
    let out = []
    for x in items
        out.push(f(x))
    return out

let k = 3
print(apply([1, 2, 3], |x| x * k))   # [3, 6, 9]
```

Assigning to a captured variable is a compile-time error (it would only
change the closure's copy): keep shared state in a list, map, struct or a
global. A nested function can call itself recursively. A lambda's body is a
single expression; use a nested `fn` for statements.

## Modules

A program can be split into files in the same folder:

```arcscript
# geometry.as
struct Point
    x
    y

fn distance(a, b)
    return sqrt(float((a.x - b.x) * (a.x - b.x) + (a.y - b.y) * (a.y - b.y)))
```

```arcscript
# main.as
import geometry                     # use as geometry.name
import geometry.{distance}          # or bring names in directly

let a = geometry.Point(0, 0)
let b = geometry.Point(x: 3, y: 4)
print(distance(a, b))
```

* `import name` loads `name.as` next to the importing file (each module is
  loaded once, even when several files import it).
* A module's top-level code runs once, when it is first imported; its
  top-level `let`s are its variables (`geometry.count`).
* Everything declared at the top level of a module is public: functions,
  structs, enums and variables. Modules can import other modules.
* Only the main file may declare the `view`.
* The standard library modules (`math`, `random`, `time`, `fs`, `path`,
  `json`, `os`, `http`) are imported the same way and are built in; see
  [STDLIB.md](STDLIB.md).

## Builtins

These are always available. Strings, lists, input and the importable
modules are described in the [standard library reference](STDLIB.md).

| Function            | Description                                 |
|---------------------|---------------------------------------------|
| `print(values...)`  | print values separated by spaces            |
| `len(x)`            | length of string (chars), list or range     |
| `str(x)` `int(x)` `float(x)` | conversions                        |
| `push(list, v)` `pop(list)` | list mutation                       |
| `join(list, sep)`   | join items into a string                    |
| `contains(h, n)`    | substring, element or map key test          |
| `get(c, key)`       | `Some(value)` for a map key or list index, else `None` |
| `keys(m)` `values(m)` | lists of a map's keys / values, in key order |
| `remove(c, key)`    | remove a map key or list index; `Some(value)` or `None` |
| `insert(list, i, v)` | insert into a list at index `i`            |
| `copy(x)`           | shallow copy of a list, map or struct       |
| `unwrap` `expect` `unwrap_or` `ok_or` `is_some` `is_none` `is_ok` `is_err` | see [Option, Result and errors](#option-result-and-errors) |
| `panic(msg)` `assert(cond, msg?)` | stop the program with an error |
| `type_of(x)`        | type name (`"int"`, `"map"`, a struct or enum name) |
| `abs` `min` `max` `sqrt` | math; `min(list)` / `max(list)` too   |
| `clock()`           | seconds since program start                 |

## Declarative, reactive UI

A program may declare one top-level `view`. After the top-level code runs,
the view is evaluated to a UI tree; **after every event it is re-evaluated**,
so the UI always reflects the current state: there is no manual invalidation.

```arcscript
let count = 0

fn add(n)
    count += n

view
    window "Counter" accent: "#6750a4"
        card padding: 24, align: "center"
            label "{count}" size: 48, weight: "bold"
            row
                button "Less" icon: "remove" -> add(-1)
                button "More" icon: "add", style: "filled" -> add(1)
            if count > 10
                label "That's a lot!" color: "tertiary"
```

### Syntax

```text
element [argument] [name: value, ...] [-> handler]
    name: value              attribute lines (any element)
    child elements           containers only
```

* Attributes can be written on the element line, comma separated, or on
  their own lines in the element's block. They must come before `->`.
  Attribute values are ordinary expressions: `selected: page == "home"`.
* Attribute lines inside `if`/`for` apply conditionally, and a function
  called from inside an element can set attributes on it, so components and
  "styles" are just functions:

  ```arcscript
  fn primary()
      style: "filled"
      size: 16

  view
      button "Save" -> save()
          primary()
  ```

* Element names are **contextual keywords**: `let label = "x"` and
  `image.width()` still treat them as ordinary names.
* Normal control flow and function calls work inside a view; a function that
  creates elements is a reusable component.
* Mistakes are caught at compile time when possible (unknown attribute,
  attribute on the wrong element, literal of the wrong type, unknown color or
  icon, with "did you mean" hints) and otherwise reported as runtime errors.

### Elements

| Element    | Argument            | Children | Handler          | Description |
|------------|---------------------|----------|------------------|-------------|
| `window`   | title (optional)    | yes      | —                | application window; sets the color scheme of its content |
| `column`   | —                   | yes      | —                | stacks children vertically |
| `row`      | —                   | yes      | —                | places children side by side, vertically centered |
| `card`     | —                   | yes      | —                | a surface grouping related content |
| `grid`     | —                   | yes      | —                | equal-width cells, `columns` per row |
| `scroll`   | —                   | yes      | —                | vertically scrollable area (`height` = maximum height) |
| `label`    | any value           | no       | —                | text |
| `button`   | any value (text)    | no       | click            | a button; `""` with an `icon` makes an icon button |
| `image`    | asset path (string) | no       | —                | PNG or JPEG from the project's `assets/` folder |
| `input`    | string              | no       | change           | text field |
| `checkbox` | bool                | no       | change           | check box |
| `switch`   | bool                | no       | change           | on/off switch |
| `slider`   | int or float        | no       | change           | number between `min` and `max` (an int slider yields ints) |
| `progress` | number 0.0–1.0      | no       | —                | progress bar |
| `spacer`   | number (optional)   | no       | —                | `spacer 16` is a fixed gap; a bare `spacer` pushes the rest of a row (or of a column with a fixed `height`) to the far end |
| `divider`  | —                   | no       | —                | thin separating line |
| `group`    | —                   | yes      | —                | children as joined segments: round outer corners, small inner corners (lists, menus); buttons and accordions become full-width segments |
| `split`    | —                   | yes      | —                | children side by side as joined panels of equal height (sidebar and content); a child's `width` is its panel's width |
| `accordion`| title (any value)   | yes      | —                | a header that shows or hides its children when clicked |

### Events and two-way binding

* `button "Ok" -> f(a, b)` binds the argument **values at render time** and
  calls `f(a, b)` when clicked. A bare `-> f` calls `f()`.
* Elements with a *change* event (`input`, `checkbox`, `switch`, `slider`)
  call their handler with the **new value appended**: `slider v -> set(1)`
  calls `set(1, new_value)`; `-> on_change` calls `on_change(new_value)`.
* Without a handler, these elements are **two-way bound** to their argument,
  which must be a global variable or an indexed element:

  ```arcscript
  let name = ""
  let done = [false, true]

  view
      input name placeholder: "Your name"     # typing updates `name`
      checkbox done[0] text: "First task"     # clicking updates done[0]
  ```

  Struct fields and map entries can be bound too: `checkbox todo.done`,
  `input form["email"]`. Binding a local variable is a compile error
  (locals are recreated on every render).
* A method call works as a handler: `button "Done" -> todo.toggle()`
  calls the method on the value `todo` had when the view was rendered.

### Attributes

| Attribute      | Type        | Elements | Meaning |
|----------------|-------------|----------|---------|
| `width`        | number      | containers, label, button, image, input, slider, progress | fixed width (initial size for `window`) |
| `height`       | number      | window, column, row, card, scroll, button, image, input, progress | fixed height (`scroll`: maximum; `progress`: thickness) |
| `stretch`      | bool        | column, row, card, scroll, button, image, input, slider, progress | fill the available width |
| `padding`      | number      | containers, button | space inside the border |
| `gap`          | number      | containers | space between children |
| `align`        | `"start"` `"center"` `"end"` | window, column, row, card, scroll | cross-axis alignment of children |
| `fill`         | color       | containers, button, input | background |
| `radius`       | number      | column, row, card, button, image, input, progress | corner radius |
| `border`       | color       | column, row, card, button, input, image | border color |
| `border_width` | number      | same as `border` | border thickness (default 1) |
| `elevation`    | int 0–5     | card, button | shadow depth |
| `style`        | see below   | button, card | visual variant |
| `size`         | number      | label, button, input | font size (default 14) |
| `color`        | color       | label, button, input, checkbox, switch, slider, progress, divider | foreground / accent color |
| `weight`       | `"regular"` `"medium"` `"bold"` | label, button | font weight |
| `italic` `mono` `wrap` | bool | label (`mono` also input) | italic, monospaced, wrapping (default on) |
| `icon`         | icon name   | label, button, input | Material icon before the text |
| `selected`     | bool        | button | highlighted state (tabs, segmented choices) |
| `enabled`      | bool        | button, input, checkbox, switch, slider | `false` greys out and ignores input |
| `tooltip`      | any value   | most elements | text shown on hover |
| `placeholder`  | any value   | input | hint while empty |
| `password`     | bool        | input | hide typed characters |
| `lines`        | int         | input | visible lines; more than 1 accepts newlines |
| `min` `max` `step` | number  | slider | range (default 0–1, or 0–100 for int sliders) and increment (0 = continuous) |
| `text`         | any value   | checkbox, switch, slider, progress | caption |
| `columns`      | int 1–64    | grid | cells per row (default 2) |
| `theme`        | `"arc"` `"dark"` `"light"` | window | arc glass (default) or Material 3 dark / light |
| `accent`       | hex color   | window | accent color (arc), seed of the whole palette (Material) |
| `tint`         | color       | image | multiplies the image colors |
| `open`         | bool        | accordion | starts open (default false); afterwards the user opens and closes it |

Button styles: `"filled"` (primary action), `"tonal"` (default), `"outlined"`,
`"text"`, `"elevated"`, `"light"` (light surface with dark text) and
`"frosted"` (translucent light glass with dark text). Buttons are pills;
`radius: 13` gives a rounded rectangle. Card styles: `"elevated"` (default), `"filled"`,
`"outlined"`.

### Colors and themes

Windows use the **arc** theme by default: the look of Arc Desktop. The
window body is dark glass that the desktop blurs, the content sits on an
inset panel (radius 14), text is white (secondary text at 60 %), buttons are
pills, cards are glass with a hairline border and radius 30, and the accent
is white, or the window's `accent` when it sets one. See
[ARC_DESKTOP.md](ARC_DESKTOP.md) for how apps integrate with the desktop.

`theme: "dark"` and `theme: "light"` switch to Material 3 instead: a
complete color scheme derived from the `accent` (computed in the perceptual
OKLCH space, contrast-checked).

In the arc theme the roles below map to its palette: `on-surface` is white,
`on-surface-variant` white at 60 %, `surface-container-low` …
`surface-container-highest` are white at 5, 8, 13 and 20 % (glass layers),
`surface-container-lowest` is a dark well (inputs), `surface-bright` is the
content panel color and `outline-variant` / `outline` are white hairlines
at 10 and 18 %. Colors with an opacity blend like in CSS.

Color attributes accept:

* hex colors: `"#rgb"`, `"#rgba"`, `"#rrggbb"`, `"#rrggbbaa"`;
* theme roles, which follow the accent and the theme: `primary`,
  `on-primary`, `primary-container`, `on-primary-container`, the same four
  for `secondary`, `tertiary` and `error`, `surface`, `on-surface`,
  `on-surface-variant`, `surface-dim`, `surface-bright`,
  `surface-container-lowest` … `surface-container-highest`, `outline`,
  `outline-variant`, `inverse-surface`, `inverse-on-surface`,
  `inverse-primary`, `shadow`, `scrim`;
* `"transparent"`, and any of the above with an opacity: `"primary/40"`,
  `"#000000/50%"`.

Prefer theme roles over hex colors: the app then re-themes itself when the
accent or the theme changes, and stays legible in every theme.

### Icons and fonts

`icon:` takes any of the ~2 200 [Material Icons](https://fonts.google.com/icons)
names (`"home"`, `"settings"`, `"play_arrow"`, `"favorite_border"`, ...).
Text uses the bundled Google Sans Flex typeface in three weights. Both fonts are
embedded in the runtime, so apps look identical on every machine.

### Images

`image "logo.png"` loads `assets/logo.png` of an `Arc.toml` project; the
files are packaged into the `.arc`/`.exe`. With only `width` or `height` the
aspect ratio is kept; with both (or `stretch: true` and a `height`) the image
is cropped to fill. `arcc` warns at build time about literal paths that are
not in the assets.

## Scoping

* `let` at file top level declares a **global** (a module variable in an
  imported file); everywhere else a block-local.
* Functions see globals, their own locals and, for nested functions and
  lambdas, the captured variables of enclosing functions.
* Name lookup: locals → captured variables → globals → functions → types →
  `Some`/`None`/`Ok`/`Err` → builtins.
