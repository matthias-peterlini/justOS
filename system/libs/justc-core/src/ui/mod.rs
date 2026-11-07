//! The declarative UI vocabulary: elements, their arguments, events and
//! attributes, attribute value types, theme colors and icons.
//!
//! This module is the single source of truth shared by the parser and the
//! compiler (static checks), the VM (runtime checks), the renderers and the
//! IDE (highlighting and documentation).
//!
//! ```justscript
//! card style: "outlined", padding: 20
//!     label "Settings" size: 22, weight: "bold"
//!     switch dark text: "Dark theme"
//!     button "Save" icon: "save" -> save()
//! ```

use crate::prelude::*;
mod color;
mod icons;

pub use color::{Color, ColorToken};

use crate::suggest::best_match;

// ---------------------------------------------------------------- elements

/// A declarative UI element. The discriminant is the bytecode encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum UiKind {
    Window = 0,
    Column = 1,
    Row = 2,
    Label = 3,
    Button = 4,
    Card = 5,
    Grid = 6,
    Scroll = 7,
    Image = 8,
    Input = 9,
    Checkbox = 10,
    Switch = 11,
    Slider = 12,
    Progress = 13,
    Spacer = 14,
    Divider = 15,
    Group = 16,
    Split = 17,
    Accordion = 18,
}

/// Type of an element's positional argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgType {
    /// Any value, shown with its display form.
    Text,
    /// A string (asset paths, input contents).
    Str,
    Bool,
    /// An `int` or a `float`.
    Number,
}

impl ArgType {
    pub fn describe(self) -> &'static str {
        match self {
            ArgType::Text => "a value",
            ArgType::Str => "a string",
            ArgType::Bool => "a bool",
            ArgType::Number => "a number",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arg {
    None,
    Optional(ArgType),
    Required(ArgType),
}

/// What the `-> handler` of an element reacts to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The element takes no handler.
    None,
    /// Called with the bound arguments when clicked.
    Click,
    /// Called with the bound arguments followed by the new value. Without a
    /// handler the element is two-way bound to its argument, which must then
    /// be a global variable or an indexed element.
    Change,
}

#[derive(Debug)]
pub struct KindSpec {
    pub kind: UiKind,
    pub name: &'static str,
    pub container: bool,
    pub arg: Arg,
    pub event: Event,
    pub summary: &'static str,
}

const fn kind(kind: UiKind, name: &'static str, container: bool, arg: Arg, event: Event, summary: &'static str) -> KindSpec {
    KindSpec { kind, name, container, arg, event, summary }
}

use Arg::{None as NoArg, Optional, Required};

/// Indexed by `UiKind as usize`.
pub const KINDS: [KindSpec; 19] = [
    kind(UiKind::Window, "window", true, Optional(ArgType::Text), Event::None, "application window; sets the theme of everything inside"),
    kind(UiKind::Column, "column", true, NoArg, Event::None, "stacks children vertically"),
    kind(UiKind::Row, "row", true, NoArg, Event::None, "places children side by side"),
    kind(UiKind::Label, "label", false, Required(ArgType::Text), Event::None, "a piece of text"),
    kind(UiKind::Button, "button", false, Required(ArgType::Text), Event::Click, "a clickable button"),
    kind(UiKind::Card, "card", true, NoArg, Event::None, "a surface that groups related content"),
    kind(UiKind::Grid, "grid", true, NoArg, Event::None, "places children in a grid of `columns` columns"),
    kind(UiKind::Scroll, "scroll", true, NoArg, Event::None, "a vertically scrollable area"),
    kind(UiKind::Image, "image", false, Required(ArgType::Str), Event::None, "a PNG or JPEG image from the package assets"),
    kind(UiKind::Input, "input", false, Required(ArgType::Str), Event::Change, "a text field"),
    kind(UiKind::Checkbox, "checkbox", false, Required(ArgType::Bool), Event::Change, "a check box"),
    kind(UiKind::Switch, "switch", false, Required(ArgType::Bool), Event::Change, "an on/off switch"),
    kind(UiKind::Slider, "slider", false, Required(ArgType::Number), Event::Change, "picks a number between `min` and `max`"),
    kind(UiKind::Progress, "progress", false, Required(ArgType::Number), Event::None, "a progress bar, from 0.0 to 1.0"),
    kind(
        UiKind::Spacer,
        "spacer",
        false,
        Optional(ArgType::Number),
        Event::None,
        "`spacer 16` is a fixed gap; a bare `spacer` pushes the following elements to the end of a row (or of a column with a fixed height)",
    ),
    kind(UiKind::Divider, "divider", false, NoArg, Event::None, "a thin separating line"),
    kind(
        UiKind::Group,
        "group",
        true,
        NoArg,
        Event::None,
        "stacks children as joined segments: round outer corners, small inner corners (lists, menus)",
    ),
    kind(
        UiKind::Split,
        "split",
        true,
        NoArg,
        Event::None,
        "places children side by side as joined panels (sidebar and content); a child's `width` fixes its panel",
    ),
    kind(UiKind::Accordion, "accordion", true, Required(ArgType::Text), Event::None, "a header that shows or hides its children when clicked"),
];

impl UiKind {
    pub const ALL: [UiKind; 19] = {
        let mut all = [UiKind::Window; 19];
        let mut i = 0;
        while i < KINDS.len() {
            all[i] = KINDS[i].kind;
            i += 1;
        }
        all
    };

    #[inline]
    pub fn spec(self) -> &'static KindSpec {
        &KINDS[self as usize]
    }

    pub fn from_u8(v: u8) -> Option<UiKind> {
        KINDS.get(v as usize).map(|s| s.kind)
    }

    pub fn from_name(name: &str) -> Option<UiKind> {
        KINDS.iter().find(|s| s.name == name).map(|s| s.kind)
    }

    #[inline]
    pub fn name(self) -> &'static str {
        self.spec().name
    }

    #[inline]
    pub fn is_container(self) -> bool {
        self.spec().container
    }

    #[inline]
    pub fn arg(self) -> Arg {
        self.spec().arg
    }

    #[inline]
    pub fn event(self) -> Event {
        self.spec().event
    }

    /// Attributes accepted by this element.
    pub fn attrs(self) -> impl Iterator<Item = Attr> {
        Attr::ALL.into_iter().filter(move |a| a.applies_to(self))
    }

    /// The containers that existed in format version 1 (`UiOpen` operand).
    pub fn is_legacy_container(self) -> bool {
        matches!(self, UiKind::Window | UiKind::Column | UiKind::Row)
    }
}

// -------------------------------------------------------------- attributes

/// An element attribute such as `padding: 16`. The discriminant is the
/// bytecode encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Attr {
    Width = 0,
    Height = 1,
    Padding = 2,
    Gap = 3,
    Align = 4,
    Fill = 5,
    Radius = 6,
    Border = 7,
    BorderWidth = 8,
    Elevation = 9,
    Style = 10,
    Size = 11,
    Color = 12,
    Weight = 13,
    Italic = 14,
    Mono = 15,
    Wrap = 16,
    Icon = 17,
    Selected = 18,
    Enabled = 19,
    Tooltip = 20,
    Placeholder = 21,
    Password = 22,
    Lines = 23,
    Min = 24,
    Max = 25,
    Step = 26,
    Text = 27,
    Columns = 28,
    Theme = 29,
    Accent = 30,
    Tint = 31,
    Stretch = 32,
    Open = 33,
}

/// Value type of an attribute.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AttrType {
    /// A finite number in `min..=max`.
    Number {
        min: f64,
        max: f64,
    },
    /// An `int` in `min..=max`.
    Integer {
        min: i64,
        max: i64,
    },
    Bool,
    /// A color string: `#rrggbb`, `#rrggbbaa` or a theme color.
    Color,
    /// Any value, shown with its display form.
    Text,
    /// One of a fixed set of strings.
    Choice(&'static [&'static str]),
    /// The name of a Material icon, such as `"home"`.
    Icon,
}

#[derive(Debug)]
pub struct AttrSpec {
    pub attr: Attr,
    pub name: &'static str,
    pub ty: AttrType,
    /// Bit `k` is set when the attribute applies to `UiKind` `k`.
    kinds: u32,
    pub summary: &'static str,
}

const fn kinds(list: &[UiKind]) -> u32 {
    let mut bits = 0;
    let mut i = 0;
    while i < list.len() {
        bits |= 1 << list[i] as u32;
        i += 1;
    }
    bits
}

const fn attr(attr: Attr, name: &'static str, ty: AttrType, kinds: u32, summary: &'static str) -> AttrSpec {
    AttrSpec { attr, name, ty, kinds, summary }
}

use UiKind as K;

const CONTAINERS: u32 = kinds(&[K::Window, K::Column, K::Row, K::Card, K::Grid, K::Scroll, K::Group, K::Split, K::Accordion]);
const LENGTH: AttrType = AttrType::Number { min: 0.0, max: 100_000.0 };
const SPACING: AttrType = AttrType::Number { min: 0.0, max: 1_000.0 };
const ANY_NUMBER: AttrType = AttrType::Number { min: f64::MIN, max: f64::MAX };

pub const STYLES: &[&str] = &["filled", "tonal", "outlined", "text", "elevated", "light", "frosted"];
pub const CARD_STYLES: &[&str] = &["filled", "outlined", "elevated"];
pub const ALIGNMENTS: &[&str] = &["start", "center", "end"];
pub const WEIGHTS: &[&str] = &["regular", "medium", "bold"];
pub const THEMES: &[&str] = &["just", "dark", "light"];

/// Indexed by `Attr as usize`.
pub const ATTRS: [AttrSpec; 34] = [
    attr(
        Attr::Width,
        "width",
        LENGTH,
        CONTAINERS | kinds(&[K::Label, K::Button, K::Image, K::Input, K::Slider, K::Progress]),
        "fixed width in points (initial width for `window`)",
    ),
    attr(
        Attr::Height,
        "height",
        LENGTH,
        kinds(&[K::Window, K::Column, K::Row, K::Card, K::Scroll, K::Button, K::Image, K::Input, K::Progress, K::Split]),
        "fixed height in points (maximum height for `scroll`, initial height for `window`)",
    ),
    attr(Attr::Padding, "padding", SPACING, CONTAINERS | kinds(&[K::Button]), "space between the border and the content"),
    attr(Attr::Gap, "gap", SPACING, CONTAINERS, "space between children"),
    attr(
        Attr::Align,
        "align",
        AttrType::Choice(ALIGNMENTS),
        kinds(&[K::Window, K::Column, K::Row, K::Card, K::Scroll, K::Accordion]),
        "cross-axis alignment of the children",
    ),
    attr(Attr::Fill, "fill", AttrType::Color, CONTAINERS | kinds(&[K::Button, K::Input]), "background color"),
    attr(
        Attr::Radius,
        "radius",
        SPACING,
        kinds(&[K::Column, K::Row, K::Card, K::Button, K::Image, K::Input, K::Progress, K::Group, K::Split]),
        "corner radius (outer corners for `group` and `split`)",
    ),
    attr(
        Attr::Border,
        "border",
        AttrType::Color,
        kinds(&[K::Column, K::Row, K::Card, K::Button, K::Input, K::Image, K::Group, K::Split]),
        "border color",
    ),
    attr(
        Attr::BorderWidth,
        "border_width",
        AttrType::Number { min: 0.0, max: 100.0 },
        kinds(&[K::Column, K::Row, K::Card, K::Button, K::Input, K::Image, K::Group, K::Split]),
        "border thickness (1 by default)",
    ),
    attr(Attr::Elevation, "elevation", AttrType::Integer { min: 0, max: 5 }, kinds(&[K::Card, K::Button]), "shadow depth, 0 to 5"),
    attr(Attr::Style, "style", AttrType::Choice(STYLES), kinds(&[K::Button, K::Card]), "visual variant"),
    attr(Attr::Size, "size", AttrType::Number { min: 4.0, max: 512.0 }, kinds(&[K::Label, K::Button, K::Input]), "font size"),
    attr(
        Attr::Color,
        "color",
        AttrType::Color,
        kinds(&[K::Label, K::Button, K::Input, K::Checkbox, K::Switch, K::Slider, K::Progress, K::Divider]),
        "foreground color",
    ),
    attr(Attr::Weight, "weight", AttrType::Choice(WEIGHTS), kinds(&[K::Label, K::Button]), "font weight"),
    attr(Attr::Italic, "italic", AttrType::Bool, kinds(&[K::Label]), "italic text"),
    attr(Attr::Mono, "mono", AttrType::Bool, kinds(&[K::Label, K::Input]), "monospaced font"),
    attr(Attr::Wrap, "wrap", AttrType::Bool, kinds(&[K::Label]), "wrap long text (true by default)"),
    attr(Attr::Icon, "icon", AttrType::Icon, kinds(&[K::Label, K::Button, K::Input, K::Accordion]), "Material icon shown before the text"),
    attr(Attr::Selected, "selected", AttrType::Bool, kinds(&[K::Button]), "highlights the button, e.g. the active tab"),
    attr(
        Attr::Enabled,
        "enabled",
        AttrType::Bool,
        kinds(&[K::Button, K::Input, K::Checkbox, K::Switch, K::Slider]),
        "false greys the element out and ignores input",
    ),
    attr(
        Attr::Tooltip,
        "tooltip",
        AttrType::Text,
        kinds(&[K::Label, K::Button, K::Image, K::Input, K::Checkbox, K::Switch, K::Slider, K::Progress]),
        "text shown when hovering",
    ),
    attr(Attr::Placeholder, "placeholder", AttrType::Text, kinds(&[K::Input]), "hint shown while the field is empty"),
    attr(Attr::Password, "password", AttrType::Bool, kinds(&[K::Input]), "hide the typed characters"),
    attr(Attr::Lines, "lines", AttrType::Integer { min: 1, max: 1_000 }, kinds(&[K::Input]), "visible lines; more than 1 allows newlines"),
    attr(Attr::Min, "min", ANY_NUMBER, kinds(&[K::Slider]), "smallest value (0 by default)"),
    attr(Attr::Max, "max", ANY_NUMBER, kinds(&[K::Slider]), "largest value (1, or 100 for integer sliders, by default)"),
    attr(Attr::Step, "step", AttrType::Number { min: 0.0, max: f64::MAX }, kinds(&[K::Slider]), "value increment; 0 is continuous"),
    attr(Attr::Text, "text", AttrType::Text, kinds(&[K::Checkbox, K::Switch, K::Slider, K::Progress]), "caption shown with the element"),
    attr(Attr::Columns, "columns", AttrType::Integer { min: 1, max: 64 }, kinds(&[K::Grid]), "number of columns (2 by default)"),
    attr(
        Attr::Theme,
        "theme",
        AttrType::Choice(THEMES),
        kinds(&[K::Window]),
        "\"just\" (default: the dark glass of the justOS desktop) or Material \"dark\" / \"light\"",
    ),
    attr(Attr::Accent, "accent", AttrType::Color, kinds(&[K::Window]), "accent color (just) or seed of the whole palette (Material)"),
    attr(Attr::Tint, "tint", AttrType::Color, kinds(&[K::Image]), "multiplies the image colors"),
    attr(
        Attr::Stretch,
        "stretch",
        AttrType::Bool,
        kinds(&[K::Column, K::Row, K::Card, K::Scroll, K::Button, K::Image, K::Input, K::Slider, K::Progress, K::Group, K::Split]),
        "fill the available width",
    ),
    attr(Attr::Open, "open", AttrType::Bool, kinds(&[K::Accordion]), "whether the accordion starts open (false by default)"),
];

/// A value about to be assigned to an attribute, independent of the VM's
/// value representation (also used for compile-time literal checks).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RawValue<'a> {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(&'a str),
    /// A value of another type; carries the type name for messages.
    Other(&'static str),
}

impl RawValue<'_> {
    fn type_name(&self) -> &'static str {
        match self {
            RawValue::Int(_) => "int",
            RawValue::Float(_) => "float",
            RawValue::Bool(_) => "bool",
            RawValue::Str(_) => "string",
            RawValue::Other(t) => t,
        }
    }
}

/// A checked attribute value.
#[derive(Clone, Debug, PartialEq)]
pub enum AttrValue {
    Number(f64),
    Bool(bool),
    Color(Color),
    Text(String),
    Choice(&'static str),
    Icon(char),
}

impl Attr {
    pub const ALL: [Attr; 34] = {
        let mut all = [Attr::Width; 34];
        let mut i = 0;
        while i < ATTRS.len() {
            all[i] = ATTRS[i].attr;
            i += 1;
        }
        all
    };

    #[inline]
    pub fn spec(self) -> &'static AttrSpec {
        &ATTRS[self as usize]
    }

    pub fn from_u8(v: u8) -> Option<Attr> {
        ATTRS.get(v as usize).map(|s| s.attr)
    }

    pub fn from_name(name: &str) -> Option<Attr> {
        ATTRS.iter().find(|s| s.name == name).map(|s| s.attr)
    }

    #[inline]
    pub fn name(self) -> &'static str {
        self.spec().name
    }

    #[inline]
    pub fn applies_to(self, kind: UiKind) -> bool {
        self.spec().kinds & (1 << kind as u32) != 0
    }

    /// Closest attribute name, for "did you mean" hints.
    pub fn suggest(name: &str) -> Option<&'static str> {
        best_match(name, ATTRS.iter().map(|s| s.name))
    }

    /// Error message for an attribute that `kind` does not accept.
    pub fn not_applicable(self, kind: UiKind) -> String {
        format!("`{}` has no attribute `{}`", kind.name(), self.name())
    }

    /// Type-check `value` for this attribute on `kind`.
    pub fn coerce(self, kind: UiKind, value: RawValue<'_>) -> Result<AttrValue, String> {
        let name = self.name();
        let mismatch = |expected: &str| format!("`{name}` expects {expected}, found {}", value.type_name());
        match self.spec().ty {
            AttrType::Number { min, max } => {
                let v = match value {
                    RawValue::Int(i) => i as f64,
                    RawValue::Float(f) => f,
                    _ => return Err(mismatch("a number")),
                };
                if !v.is_finite() {
                    return Err(format!("`{name}` must be a finite number, found {v}"));
                }
                if v < min || v > max {
                    return Err(format!("`{name}` must be between {} and {}, found {v}", fmt_bound(min), fmt_bound(max)));
                }
                Ok(AttrValue::Number(v))
            }
            AttrType::Integer { min, max } => match value {
                RawValue::Int(i) if (min..=max).contains(&i) => Ok(AttrValue::Number(i as f64)),
                RawValue::Int(i) => Err(format!("`{name}` must be between {min} and {max}, found {i}")),
                _ => Err(mismatch("an int")),
            },
            AttrType::Bool => match value {
                RawValue::Bool(b) => Ok(AttrValue::Bool(b)),
                _ => Err(mismatch("a bool")),
            },
            AttrType::Color => {
                let RawValue::Str(s) = value else { return Err(mismatch("a color string such as \"#6750a4\" or \"primary\"")) };
                let color = Color::parse(s).map_err(|e| format!("`{name}`: {e}"))?;
                if self == Attr::Accent && !matches!(color, Color::Rgba(_)) {
                    return Err(format!("`{name}` must be a literal color such as \"#6750a4\", not a theme color"));
                }
                Ok(AttrValue::Color(color))
            }
            AttrType::Text => Ok(AttrValue::Text(match value {
                RawValue::Int(i) => i.to_string(),
                RawValue::Float(f) => f.to_string(),
                RawValue::Bool(b) => b.to_string(),
                RawValue::Str(s) => s.to_owned(),
                RawValue::Other(t) => return Err(format!("`{name}` cannot display a value of type {t}")),
            })),
            AttrType::Choice(choices) => {
                let choices = if self == Attr::Style && kind == UiKind::Card { CARD_STYLES } else { choices };
                let RawValue::Str(s) = value else { return Err(mismatch(&format!("one of {}", quoted_list(choices)))) };
                match choices.iter().find(|c| **c == s) {
                    Some(c) => Ok(AttrValue::Choice(c)),
                    None => Err(format!("invalid `{name}` {s:?} for `{}`: expected one of {}", kind.name(), quoted_list(choices))),
                }
            }
            AttrType::Icon => {
                let RawValue::Str(s) = value else { return Err(mismatch("an icon name such as \"home\"")) };
                icon(s).map(AttrValue::Icon).ok_or_else(|| {
                    let mut msg = format!("unknown icon {s:?}");
                    if let Some(similar) = suggest_icon(s) {
                        msg.push_str(&format!(" (did you mean {similar:?}?)"));
                    }
                    msg
                })
            }
        }
    }
}

fn fmt_bound(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        v.to_string()
    }
}

fn quoted_list(items: &[&str]) -> String {
    items.iter().map(|s| format!("{s:?}")).collect::<Vec<_>>().join(", ")
}

// ------------------------------------------------------------------- icons

/// Codepoint of a Material icon in the bundled icon font.
pub fn icon(name: &str) -> Option<char> {
    icons::ICONS.binary_search_by(|(n, _)| (*n).cmp(name)).ok().map(|i| icons::ICONS[i].1)
}

/// All icon names, sorted.
pub fn icon_names() -> impl Iterator<Item = &'static str> {
    icons::ICONS.iter().map(|(n, _)| *n)
}

fn suggest_icon(name: &str) -> Option<&'static str> {
    best_match(name, icon_names())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_indexed_by_discriminant() {
        for (i, s) in KINDS.iter().enumerate() {
            assert_eq!(s.kind as usize, i, "{}", s.name);
            assert_eq!(UiKind::from_name(s.name), Some(s.kind));
        }
        for (i, s) in ATTRS.iter().enumerate() {
            assert_eq!(s.attr as usize, i, "{}", s.name);
            assert_eq!(Attr::from_name(s.name), Some(s.attr));
            assert_ne!(s.kinds, 0, "`{}` applies to nothing", s.name);
        }
        // An element name never doubles as an attribute name: `name:` at the
        // start of a line is always an attribute.
        for s in &KINDS {
            assert!(Attr::from_name(s.name).is_none(), "{}", s.name);
        }
        assert!(UiKind::from_u8(KINDS.len() as u8).is_none() && Attr::from_u8(ATTRS.len() as u8).is_none());
    }

    #[test]
    fn icons_are_sorted_and_found() {
        assert!(icons::ICONS.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(icon("home"), Some('\u{e88a}'));
        assert_eq!(icon("no_such_icon"), None);
        let err = Attr::Icon.coerce(UiKind::Button, RawValue::Str("hom")).unwrap_err();
        assert!(err.contains("did you mean"), "{err}");
    }

    #[test]
    fn coercion_checks_types_and_ranges() {
        let b = UiKind::Button;
        assert_eq!(Attr::Padding.coerce(b, RawValue::Int(8)), Ok(AttrValue::Number(8.0)));
        assert!(Attr::Padding.coerce(b, RawValue::Int(-1)).unwrap_err().contains("between 0 and 1000"));
        assert!(Attr::Padding.coerce(b, RawValue::Str("8")).unwrap_err().contains("expects a number, found string"));
        assert!(Attr::Elevation.coerce(b, RawValue::Float(1.5)).unwrap_err().contains("expects an int"));
        assert_eq!(Attr::Style.coerce(b, RawValue::Str("tonal")), Ok(AttrValue::Choice("tonal")));
        assert!(Attr::Style.coerce(UiKind::Card, RawValue::Str("tonal")).is_err());
        assert_eq!(Attr::Tooltip.coerce(b, RawValue::Int(3)), Ok(AttrValue::Text("3".into())));
        assert!(Attr::Accent.coerce(UiKind::Window, RawValue::Str("primary")).is_err());
        assert!(Attr::Accent.coerce(UiKind::Window, RawValue::Str("#6750a4")).is_ok());
        assert!(Attr::Fill.coerce(b, RawValue::Str("primay")).unwrap_err().contains("primary"));
        assert!(Attr::Width.coerce(b, RawValue::Float(f64::NAN)).is_err());
    }

    #[test]
    fn applicability() {
        assert!(Attr::Placeholder.applies_to(UiKind::Input));
        assert!(!Attr::Placeholder.applies_to(UiKind::Label));
        assert!(UiKind::Card.attrs().any(|a| a == Attr::Elevation));
        assert_eq!(Attr::suggest("paddin"), Some("padding"));
    }
}
