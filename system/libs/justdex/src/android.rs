//! The Android part: an `Activity` whose screen is built in code from
//! `TextView`, `Button`, `EditText`, `CheckBox`, `ProgressBar` and
//! `LinearLayout`, with click listeners, `Toast`, `Log` and `Handler`
//! timers.
//!
//! This module keeps the screen as a tree of objects; the program that runs
//! the interpreter draws it and passes clicks and typing back
//! ([`Vm::click`], [`Vm::type_text`]).

use crate::{java_name, new_ref, Inst, JResult, Native, Obj, Ref, Val, Vm};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

const ACTIVITY: &str = "Landroid/app/Activity;";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Plain,
    Text,
    Button,
    Edit,
    Check,
    Group,
    Progress,
    Image,
}

#[derive(Debug)]
pub struct View {
    pub kind: ViewKind,
    pub id: i32,
    pub text: String,
    pub hint: String,
    /// In sp; 0 is the default (14).
    pub text_size: f32,
    pub text_color: Option<u32>,
    pub bg: Option<u32>,
    pub children: Vec<Ref>,
    pub on_click: Val,
    pub on_checked: Val,
    pub vertical: bool,
    /// 0 visible, 4 invisible, 8 gone.
    pub visibility: i32,
    pub enabled: bool,
    pub checked: bool,
    pub gravity: i32,
    pub padding: [i32; 4],
    pub progress: i32,
    pub max: i32,
    pub bold: bool,
    pub tag: Val,
}

impl View {
    pub fn new(kind: ViewKind) -> View {
        View {
            kind,
            id: -1,
            text: String::new(),
            hint: String::new(),
            text_size: 0.0,
            text_color: None,
            bg: None,
            children: Vec::new(),
            on_click: Val::NULL,
            on_checked: Val::NULL,
            vertical: false,
            visibility: 0,
            enabled: true,
            checked: false,
            gravity: 0,
            padding: [0; 4],
            progress: 0,
            max: 100,
            bold: false,
            tag: Val::NULL,
        }
    }
}

#[derive(Debug, Default)]
pub struct Activity;

pub struct Timer {
    /// Uptime in milliseconds.
    pub due: u64,
    pub run: Val,
}

/// What the program shows.
#[derive(Default)]
pub struct Ui {
    pub activity: Option<Ref>,
    pub content: Option<Ref>,
    pub title: Option<String>,
    /// Toasts not shown yet (text, milliseconds).
    pub toasts: Vec<(String, u64)>,
    pub timers: Vec<Timer>,
    /// `finish()` was called.
    pub finished: bool,
    /// Something on the screen changed.
    pub dirty: bool,
    next_id: i32,
}

/// Library classes as a program may name them (AndroidX and support
/// libraries included): the class this module implements.
pub fn alias(class: &str) -> Option<&'static str> {
    Some(match class {
        "Landroidx/appcompat/app/AppCompatActivity;"
        | "Landroidx/activity/ComponentActivity;"
        | "Landroidx/core/app/ComponentActivity;"
        | "Landroidx/fragment/app/FragmentActivity;"
        | "Landroid/support/v7/app/AppCompatActivity;"
        | "Landroid/support/v4/app/FragmentActivity;"
        | "Landroid/app/ListActivity;" => ACTIVITY,
        "Landroidx/appcompat/widget/AppCompatTextView;" => "Landroid/widget/TextView;",
        "Landroidx/appcompat/widget/AppCompatButton;" | "Lcom/google/android/material/button/MaterialButton;" => "Landroid/widget/Button;",
        "Landroidx/appcompat/widget/AppCompatEditText;" => "Landroid/widget/EditText;",
        "Landroidx/appcompat/widget/AppCompatCheckBox;" => "Landroid/widget/CheckBox;",
        "Landroidx/appcompat/widget/SwitchCompat;" => "Landroid/widget/Switch;",
        "Landroidx/appcompat/widget/AppCompatImageView;" => "Landroid/widget/ImageView;",
        _ => return None,
    })
}

fn kind_of(class: &str) -> Option<ViewKind> {
    Some(match class {
        "Landroid/view/View;" | "Landroid/widget/Space;" => ViewKind::Plain,
        "Landroid/widget/TextView;" => ViewKind::Text,
        "Landroid/widget/Button;" | "Landroid/widget/ImageButton;" => ViewKind::Button,
        "Landroid/widget/EditText;" | "Landroid/widget/AutoCompleteTextView;" => ViewKind::Edit,
        "Landroid/widget/CheckBox;" | "Landroid/widget/Switch;" | "Landroid/widget/ToggleButton;" | "Landroid/widget/RadioButton;" | "Landroid/widget/CompoundButton;" => {
            ViewKind::Check
        }
        "Landroid/widget/LinearLayout;"
        | "Landroid/widget/FrameLayout;"
        | "Landroid/widget/RelativeLayout;"
        | "Landroid/widget/ScrollView;"
        | "Landroid/widget/HorizontalScrollView;"
        | "Landroid/widget/GridLayout;"
        | "Landroid/widget/TableLayout;"
        | "Landroid/widget/TableRow;"
        | "Landroid/widget/RadioGroup;"
        | "Landroid/view/ViewGroup;" => ViewKind::Group,
        "Landroid/widget/ProgressBar;" | "Landroid/widget/SeekBar;" => ViewKind::Progress,
        "Landroid/widget/ImageView;" => ViewKind::Image,
        _ => return None,
    })
}

pub(crate) fn supertypes(class: &str) -> &'static [&'static str] {
    match class {
        "Landroid/app/Activity;" => &["Landroid/content/ContextWrapper;"],
        "Landroid/content/ContextWrapper;" => &["Landroid/content/Context;"],
        "Landroid/widget/Button;" | "Landroid/widget/EditText;" | "Landroid/widget/CompoundButton;" => &["Landroid/widget/TextView;"],
        "Landroid/widget/CheckBox;" | "Landroid/widget/Switch;" | "Landroid/widget/ToggleButton;" | "Landroid/widget/RadioButton;" => &["Landroid/widget/CompoundButton;"],
        "Landroid/widget/SeekBar;" => &["Landroid/widget/ProgressBar;"],
        "Landroid/widget/ImageButton;" => &["Landroid/widget/ImageView;"],
        "Landroid/widget/RadioGroup;" | "Landroid/widget/TableRow;" | "Landroid/widget/TableLayout;" => &["Landroid/widget/LinearLayout;"],
        "Landroid/widget/LinearLayout;"
        | "Landroid/widget/FrameLayout;"
        | "Landroid/widget/RelativeLayout;"
        | "Landroid/widget/GridLayout;" => &["Landroid/view/ViewGroup;"],
        "Landroid/widget/ScrollView;" | "Landroid/widget/HorizontalScrollView;" => &["Landroid/widget/FrameLayout;"],
        "Landroid/widget/TextView;" | "Landroid/widget/ImageView;" | "Landroid/widget/ProgressBar;" | "Landroid/view/ViewGroup;" | "Landroid/widget/Space;" => {
            &["Landroid/view/View;"]
        }
        _ => &[],
    }
}

pub(crate) fn new_native(class: &str) -> Option<Native> {
    if let Some(k) = kind_of(class) {
        let mut v = View::new(k);
        // ScrollView and the other containers stack their children.
        v.vertical = class != "Landroid/widget/LinearLayout;" && class != "Landroid/widget/TableRow;";
        return Some(Native::View(v));
    }
    Some(match class {
        ACTIVITY => Native::Activity(Activity),
        "Landroid/widget/Toast;" => Native::Toast(String::new()),
        "Landroid/os/Handler;" => Native::Handler,
        _ => return None,
    })
}

pub(crate) fn static_field(vm: &mut Vm, class: &str, name: &str) -> Option<Val> {
    Some(match (class, name) {
        ("Landroid/os/Build$VERSION;", "SDK_INT") => Val::int(23),
        ("Landroid/os/Build$VERSION;", "RELEASE") => vm.jstring("6.0".into()),
        ("Landroid/os/Build;", "MODEL" | "DEVICE" | "PRODUCT") => vm.jstring("JustOS".into()),
        ("Landroid/os/Build;", "MANUFACTURER" | "BRAND") => vm.jstring("JustOS".into()),
        _ => return None,
    })
}

pub(crate) fn construct(vm: &mut Vm, _class: &str, sig: &str, this: &Val, args: &[Val]) -> JResult<()> {
    // Views take a Context (and attributes); handlers a Looper.
    if sig == "(Landroid/content/Context;Ljava/lang/CharSequence;I)V" {
        let s = vm.to_jstring(&args[1])?;
        if let Val::R(o) = this {
            if let Obj::Inst(Inst { native: Native::Toast(t), .. }) = &mut *o.borrow_mut() {
                *t = s;
            }
        }
    }
    Ok(())
}

/// The view inside an object, if it has one.
pub fn view(o: &Ref) -> Option<core::cell::Ref<'_, View>> {
    core::cell::Ref::filter_map(o.borrow(), |x| match x {
        Obj::Inst(Inst { native: Native::View(v), .. }) => Some(v),
        _ => None,
    })
    .ok()
}

fn with_view<R>(o: &Ref, f: impl FnOnce(&mut View) -> R) -> Option<R> {
    match &mut *o.borrow_mut() {
        Obj::Inst(Inst { native: Native::View(v), .. }) => Some(f(v)),
        _ => None,
    }
}

/// The view with `id` in the tree under `root`.
fn find(root: &Ref, id: i32) -> Option<Ref> {
    let children = {
        let v = view(root)?;
        if v.id == id {
            return Some(root.clone());
        }
        v.children.clone()
    };
    children.iter().find_map(|c| find(c, id))
}

/// `#RRGGBB`, `#AARRGGBB` or a colour name, as `Color.parseColor`.
fn parse_color(s: &str) -> Option<u32> {
    if let Some(hex) = s.strip_prefix('#') {
        let v = u32::from_str_radix(hex, 16).ok()?;
        return match hex.len() {
            6 => Some(0xFF00_0000 | v),
            8 => Some(v),
            _ => None,
        };
    }
    Some(match s.to_ascii_lowercase().as_str() {
        "black" => 0xFF00_0000,
        "white" => 0xFFFF_FFFF,
        "red" => 0xFFFF_0000,
        "green" => 0xFF00_FF00,
        "blue" => 0xFF00_00FF,
        "yellow" => 0xFFFF_FF00,
        "cyan" | "aqua" => 0xFF00_FFFF,
        "magenta" | "fuchsia" => 0xFFFF_00FF,
        "gray" | "grey" => 0xFF88_8888,
        "lightgray" | "lightgrey" => 0xFFCC_CCCC,
        "darkgray" | "darkgrey" => 0xFF44_4444,
        "transparent" => 0,
        _ => return None,
    })
}

pub(crate) fn static_method(vm: &mut Vm, class: &str, name: &str, sig: &str, args: &[Val]) -> JResult<Option<Val>> {
    let a0 = args.first().cloned().unwrap_or(Val::NULL);
    Ok(Some(match class {
        "Landroid/util/Log;" => {
            let level = match name {
                "v" => 'V',
                "d" => 'D',
                "i" => 'I',
                "w" => 'W',
                "e" => 'E',
                "wtf" => 'A',
                _ => return Ok(None),
            };
            let tag = vm.to_jstring(&a0)?;
            let msg = match args.get(1) {
                Some(m) if Vm::str_of(m).is_some() || m.is_null() => vm.to_jstring(m)?,
                Some(t) => vm.describe(&t.as_ref().cloned().unwrap_or_else(|| new_ref(Obj::Str(String::new())))),
                None => String::new(),
            };
            vm.host.write(&format!("{level}/{tag}: {msg}\n"), true);
            Val::int(0)
        }
        "Landroid/widget/Toast;" if name == "makeText" => {
            let text = if sig.contains(";II)") { format!("{}", args[1].i()) } else { vm.to_jstring(&args[1])? };
            let o = new_ref(Obj::Inst(Inst { class: Rc::from("Landroid/widget/Toast;"), fields: BTreeMap::new(), native: Native::Toast(text) }));
            if let Obj::Inst(i) = &mut *o.borrow_mut() {
                i.fields.insert(Rc::from("$long"), Val::bool(args.get(2).is_some_and(|d| d.i() == 1)));
            }
            Val::R(o)
        }
        "Landroid/os/Looper;" => match name {
            "getMainLooper" | "myLooper" => Val::R(new_ref(Obj::Inst(Inst { class: Rc::from(class), fields: BTreeMap::new(), native: Native::None }))),
            "prepare" | "loop" => Val::NULL,
            _ => return Ok(None),
        },
        "Landroid/text/TextUtils;" => match name {
            "isEmpty" => Val::bool(a0.is_null() || Vm::str_of(&a0).is_some_and(|s| s.is_empty())),
            "equals" => Val::bool(vm.jequals(&a0, &args[1])? || (a0.is_null() && args[1].is_null())),
            _ => return Ok(None),
        },
        "Landroid/graphics/Color;" => match name {
            "rgb" => Val::int((0xFF00_0000 | (args[0].i() as u32 & 0xFF) << 16 | (args[1].i() as u32 & 0xFF) << 8 | (args[2].i() as u32 & 0xFF)) as i32),
            "argb" => Val::int(((args[0].i() as u32 & 0xFF) << 24 | (args[1].i() as u32 & 0xFF) << 16 | (args[2].i() as u32 & 0xFF) << 8 | (args[3].i() as u32 & 0xFF)) as i32),
            "parseColor" => {
                let s = Vm::str_of(&a0).unwrap_or_default();
                match parse_color(&s) {
                    Some(c) => Val::int(c as i32),
                    None => return vm.throw("Ljava/lang/IllegalArgumentException;", "Unknown color"),
                }
            }
            "red" => Val::int(a0.i() >> 16 & 0xFF),
            "green" => Val::int(a0.i() >> 8 & 0xFF),
            "blue" => Val::int(a0.i() & 0xFF),
            "alpha" => Val::int((a0.i() as u32 >> 24) as i32),
            _ => return Ok(None),
        },
        "Landroid/os/SystemClock;" => match name {
            "uptimeMillis" | "elapsedRealtime" | "currentThreadTimeMillis" => Val::long(vm.host.uptime_ms() as i64),
            "sleep" => {
                vm.host.sleep(a0.j().max(0) as u64);
                Val::NULL
            }
            _ => return Ok(None),
        },
        "Landroid/view/View;" if name == "generateViewId" => {
            vm.ui.next_id += 1;
            Val::int(0x0100_0000 + vm.ui.next_id)
        }
        _ => return Ok(None),
    }))
}

pub(crate) fn method(vm: &mut Vm, o: &Ref, _class: &str, name: &str, sig: &str, args: &[Val]) -> JResult<Option<Val>> {
    let a0 = args.first().cloned().unwrap_or(Val::NULL);
    let this = Val::R(o.clone());
    let which = match &*o.borrow() {
        Obj::Inst(i) => match &i.native {
            Native::View(_) => 1,
            Native::Activity(_) => 2,
            Native::Toast(_) => 3,
            Native::Handler => 4,
            _ => 0,
        },
        _ => 0,
    };
    let now = vm.host.uptime_ms();
    Ok(Some(match which {
        1 => {
            let changed = |vm: &mut Vm| vm.ui.dirty = true;
            match name {
                "setText" | "append" | "setHint" => {
                    let s = if sig.starts_with("(I") { format!("{}", a0.i()) } else { vm.to_jstring(&a0)? };
                    with_view(o, |v| match name {
                        "setText" => v.text = s,
                        "append" => v.text.push_str(&s),
                        _ => v.hint = s,
                    });
                    changed(vm);
                    Val::NULL
                }
                "getText" | "getHint" => {
                    let s = with_view(o, |v| if name == "getText" { v.text.clone() } else { v.hint.clone() }).unwrap_or_default();
                    vm.jstring(s)
                }
                "length" => Val::int(with_view(o, |v| crate::text::len(&v.text) as i32).unwrap_or(0)),
                "setTextSize" => {
                    let size = args.last().map(|v| v.f()).unwrap_or(14.0);
                    with_view(o, |v| v.text_size = size);
                    changed(vm);
                    Val::NULL
                }
                "setTextColor" | "setBackgroundColor" => {
                    let c = a0.i() as u32;
                    with_view(o, |v| {
                        if name == "setTextColor" {
                            v.text_color = Some(c)
                        } else {
                            v.bg = Some(c)
                        }
                    });
                    changed(vm);
                    Val::NULL
                }
                "setTypeface" => {
                    // setTypeface(null, Typeface.BOLD) and friends.
                    let bold = args.get(1).is_some_and(|s| s.i() & 1 == 1);
                    with_view(o, |v| v.bold = bold);
                    changed(vm);
                    Val::NULL
                }
                "setGravity" => {
                    with_view(o, |v| v.gravity = a0.i());
                    changed(vm);
                    Val::NULL
                }
                "setPadding" | "setPaddingRelative" => {
                    let p = [a0.i(), args[1].i(), args[2].i(), args[3].i()];
                    with_view(o, |v| v.padding = p);
                    changed(vm);
                    Val::NULL
                }
                "setId" => {
                    with_view(o, |v| v.id = a0.i());
                    Val::NULL
                }
                "getId" => Val::int(with_view(o, |v| v.id).unwrap_or(-1)),
                "setTag" => {
                    with_view(o, |v| v.tag = a0.clone());
                    Val::NULL
                }
                "getTag" => with_view(o, |v| v.tag.clone()).unwrap_or(Val::NULL),
                "setOnClickListener" => {
                    with_view(o, |v| v.on_click = a0.clone());
                    Val::NULL
                }
                "setOnCheckedChangeListener" => {
                    with_view(o, |v| v.on_checked = a0.clone());
                    Val::NULL
                }
                "setVisibility" => {
                    with_view(o, |v| v.visibility = a0.i());
                    changed(vm);
                    Val::NULL
                }
                "getVisibility" => Val::int(with_view(o, |v| v.visibility).unwrap_or(0)),
                "setEnabled" => {
                    with_view(o, |v| v.enabled = a0.z());
                    changed(vm);
                    Val::NULL
                }
                "isEnabled" => Val::bool(with_view(o, |v| v.enabled).unwrap_or(true)),
                "setChecked" => {
                    let was = with_view(o, |v| core::mem::replace(&mut v.checked, a0.z())).unwrap_or(false);
                    changed(vm);
                    if was != a0.z() {
                        notify_checked(vm, o)?;
                    }
                    Val::NULL
                }
                "isChecked" => Val::bool(with_view(o, |v| v.checked).unwrap_or(false)),
                "toggle" => {
                    with_view(o, |v| v.checked = !v.checked);
                    changed(vm);
                    notify_checked(vm, o)?;
                    Val::NULL
                }
                "setProgress" | "setMax" => {
                    with_view(o, |v| {
                        if name == "setMax" {
                            v.max = a0.i().max(1)
                        } else {
                            v.progress = a0.i()
                        }
                    });
                    changed(vm);
                    Val::NULL
                }
                "getProgress" => Val::int(with_view(o, |v| v.progress).unwrap_or(0)),
                "getMax" => Val::int(with_view(o, |v| v.max).unwrap_or(100)),
                "setOrientation" => {
                    with_view(o, |v| v.vertical = a0.i() == 1);
                    changed(vm);
                    Val::NULL
                }
                "getOrientation" => Val::int(with_view(o, |v| v.vertical as i32).unwrap_or(0)),
                "addView" => {
                    let Val::R(child) = &a0 else { return vm.npe("addView(null)") };
                    let at = if sig.starts_with("(Landroid/view/View;I") { Some(args[1].i()) } else { None };
                    with_view(o, |v| match at {
                        Some(i) if i >= 0 && (i as usize) <= v.children.len() => v.children.insert(i as usize, child.clone()),
                        _ => v.children.push(child.clone()),
                    });
                    changed(vm);
                    Val::NULL
                }
                "removeView" => {
                    with_view(o, |v| v.children.retain(|c| !matches!(&a0, Val::R(x) if Rc::ptr_eq(x, c))));
                    changed(vm);
                    Val::NULL
                }
                "removeViewAt" => {
                    with_view(o, |v| {
                        if (a0.i() as usize) < v.children.len() {
                            v.children.remove(a0.i() as usize);
                        }
                    });
                    changed(vm);
                    Val::NULL
                }
                "removeAllViews" => {
                    with_view(o, |v| v.children.clear());
                    changed(vm);
                    Val::NULL
                }
                "getChildCount" => Val::int(with_view(o, |v| v.children.len() as i32).unwrap_or(0)),
                "getChildAt" => with_view(o, |v| v.children.get(a0.i().max(0) as usize).cloned()).flatten().map(Val::R).unwrap_or(Val::NULL),
                "findViewById" => find(o, a0.i()).map(Val::R).unwrap_or(Val::NULL),
                "performClick" | "callOnClick" => {
                    vm.click(o)?;
                    Val::bool(true)
                }
                "getContext" | "getRootView" => vm.ui.activity.clone().map(Val::R).unwrap_or(Val::NULL),
                "post" | "postDelayed" => {
                    let delay = if name == "postDelayed" { args[1].j().max(0) as u64 } else { 0 };
                    vm.ui.timers.push(Timer { due: now + delay, run: a0 });
                    Val::bool(true)
                }
                "removeCallbacks" => {
                    vm.ui.timers.retain(|t| !t.run.same(&a0));
                    Val::bool(true)
                }
                "invalidate" | "requestLayout" | "requestFocus" | "clearFocus" => {
                    changed(vm);
                    if name == "requestFocus" {
                        Val::bool(true)
                    } else {
                        Val::NULL
                    }
                }
                "setLayoutParams" | "setAlpha" | "setElevation" | "setMinimumHeight" | "setMinimumWidth" | "setAllCaps" | "setInputType" | "setSingleLine"
                | "setLines" | "setMaxLines" | "setError" | "setImageResource" | "setImageDrawable" | "setImageBitmap" | "setScaleType" | "setOnLongClickListener"
                | "setOnTouchListener" | "setFocusable" | "setClickable" | "setSelection" | "setBackground" | "setBackgroundResource" | "addTextChangedListener"
                | "setIndeterminate" | "setOnSeekBarChangeListener" | "setTextAlignment" | "setLetterSpacing" | "setMinWidth" | "setMinHeight" => Val::NULL,
                "getWidth" | "getHeight" | "getMeasuredWidth" | "getMeasuredHeight" => Val::int(0),
                _ => return Ok(None),
            }
        }
        2 => match name {
            "setContentView" => {
                let Val::R(v) = &a0 else {
                    return vm.throw(
                        "Ljava/lang/UnsupportedOperationException;",
                        "setContentView(R.layout...): JustOS cannot read XML layouts yet; build the screen in code",
                    );
                };
                vm.ui.content = Some(v.clone());
                vm.ui.dirty = true;
                Val::NULL
            }
            "setTitle" => {
                let t = if sig == "(I)V" { format!("{}", a0.i()) } else { vm.to_jstring(&a0)? };
                vm.ui.title = Some(t);
                vm.ui.dirty = true;
                Val::NULL
            }
            "getTitle" => {
                let t = vm.ui.title.clone().unwrap_or_default();
                vm.jstring(t)
            }
            "findViewById" => match vm.ui.content.clone() {
                Some(root) => find(&root, a0.i()).map(Val::R).unwrap_or(Val::NULL),
                None => Val::NULL,
            },
            "finish" | "finishAffinity" => {
                vm.ui.finished = true;
                Val::NULL
            }
            "isFinishing" => Val::bool(vm.ui.finished),
            "onBackPressed" => {
                vm.ui.finished = true;
                Val::NULL
            }
            "runOnUiThread" => {
                vm.call_virtual(&a0, "run", "()V", Vec::new())?;
                Val::NULL
            }
            "getApplicationContext" | "getBaseContext" | "getApplication" => this,
            "getPackageName" => vm.jstring("justos.app".into()),
            "getSystemService" | "getIntent" | "getActionBar" | "getSupportActionBar" | "getWindow" | "getResources" | "getSharedPreferences" => Val::NULL,
            n if n.starts_with("on") => Val::NULL,
            _ => return Ok(None),
        },
        3 => match name {
            "show" => {
                let (text, long) = match &*o.borrow() {
                    Obj::Inst(i) => (
                        match &i.native {
                            Native::Toast(t) => t.clone(),
                            _ => String::new(),
                        },
                        i.fields.get("$long").is_some_and(|v| v.z()),
                    ),
                    _ => (String::new(), false),
                };
                vm.ui.toasts.push((text, if long { 3500 } else { 2000 }));
                vm.ui.dirty = true;
                Val::NULL
            }
            "setText" => {
                let s = vm.to_jstring(&a0)?;
                if let Obj::Inst(Inst { native: Native::Toast(t), .. }) = &mut *o.borrow_mut() {
                    *t = s;
                }
                Val::NULL
            }
            "cancel" | "setDuration" | "setGravity" => Val::NULL,
            _ => return Ok(None),
        },
        4 => match name {
            "post" | "postDelayed" | "postAtTime" => {
                let delay = match name {
                    "postDelayed" => args[1].j().max(0) as u64,
                    "postAtTime" => (args[1].j().max(0) as u64).saturating_sub(now),
                    _ => 0,
                };
                vm.ui.timers.push(Timer { due: now + delay, run: a0 });
                Val::bool(true)
            }
            "removeCallbacks" => {
                vm.ui.timers.retain(|t| !t.run.same(&a0));
                Val::NULL
            }
            "removeCallbacksAndMessages" => {
                vm.ui.timers.clear();
                Val::NULL
            }
            _ => return Ok(None),
        },
        _ => return Ok(None),
    }))
}

fn notify_checked(vm: &mut Vm, o: &Ref) -> JResult<()> {
    let (l, checked) = with_view(o, |v| (v.on_checked.clone(), v.checked)).unwrap_or((Val::NULL, false));
    if !l.is_null() {
        vm.call_virtual(&l, "onCheckedChanged", "(Landroid/widget/CompoundButton;Z)V", alloc::vec![Val::R(o.clone()), Val::bool(checked)])?;
    }
    Ok(())
}

impl Vm {
    /// The program's main screen: an `Activity` subclass (`MainActivity`
    /// if there is one).
    pub fn activity_class(&mut self) -> Option<Rc<str>> {
        let mut found = Vec::new();
        for name in self.class_names() {
            let Some(c) = self.class(&name) else { continue };
            if c.access & (crate::dex::ACC_ABSTRACT | crate::dex::ACC_INTERFACE) != 0 {
                continue;
            }
            if name.as_ref() != ACTIVITY && self.is_subclass(&name, ACTIVITY) {
                found.push(name);
            }
        }
        let main = found.iter().find(|n| java_name(n).rsplit('.').next() == Some("MainActivity")).cloned();
        main.or_else(|| found.first().cloned())
    }

    /// Creates the activity and runs `onCreate`, `onStart` and `onResume`.
    pub fn start_activity(&mut self, class: &str) -> JResult<Ref> {
        let a = self.construct(class, "()V", Vec::new())?;
        self.ui.activity = Some(a.clone());
        let this = Val::R(a.clone());
        self.call_virtual(&this, "onCreate", "(Landroid/os/Bundle;)V", alloc::vec![Val::NULL])?;
        self.call_virtual(&this, "onStart", "()V", Vec::new())?;
        self.call_virtual(&this, "onResume", "()V", Vec::new())?;
        self.ui.dirty = true;
        Ok(a)
    }

    /// The window is closing: `onPause`, `onStop`, `onDestroy`.
    pub fn stop_activity(&mut self) -> JResult<()> {
        if let Some(a) = self.ui.activity.clone() {
            let this = Val::R(a);
            for m in ["onPause", "onStop", "onDestroy"] {
                self.call_virtual(&this, m, "()V", Vec::new())?;
            }
        }
        Ok(())
    }

    /// The back button.
    pub fn back(&mut self) -> JResult<()> {
        if let Some(a) = self.ui.activity.clone() {
            self.call_virtual(&Val::R(a), "onBackPressed", "()V", Vec::new())?;
        }
        Ok(())
    }

    /// The user clicked a view.
    pub fn click(&mut self, v: &Ref) -> JResult<()> {
        let (kind, listener) = match view(v) {
            Some(x) if x.enabled => (x.kind, x.on_click.clone()),
            _ => return Ok(()),
        };
        if kind == ViewKind::Check {
            with_view(v, |x| x.checked = !x.checked);
            self.ui.dirty = true;
            notify_checked(self, v)?;
        }
        if !listener.is_null() {
            self.call_virtual(&listener, "onClick", "(Landroid/view/View;)V", alloc::vec![Val::R(v.clone())])?;
        }
        self.ui.dirty = true;
        Ok(())
    }

    /// Typing into an `EditText`: `s` is added, or with `back` a character
    /// is taken away.
    pub fn type_text(&mut self, v: &Ref, s: &str, back: bool) {
        with_view(v, |x| {
            if back {
                x.text.pop();
            } else {
                x.text.push_str(s);
            }
        });
        self.ui.dirty = true;
    }

    /// Runs the `Handler` and `View.post` timers that are due.
    pub fn run_timers(&mut self) -> JResult<()> {
        let now = self.host.uptime_ms();
        let (due, rest): (Vec<Timer>, Vec<Timer>) = core::mem::take(&mut self.ui.timers).into_iter().partition(|t| t.due <= now);
        self.ui.timers = rest;
        for t in due {
            self.call_virtual(&t.run, "run", "()V", Vec::new())?;
            self.ui.dirty = true;
        }
        Ok(())
    }

    /// When the next timer is due (uptime in milliseconds).
    pub fn next_timer(&self) -> Option<u64> {
        self.ui.timers.iter().map(|t| t.due).min()
    }
}
