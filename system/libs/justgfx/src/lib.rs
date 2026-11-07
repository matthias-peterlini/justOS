//! Drawing for justOS, shared by the kernel's interface and by programs with
//! windows: pixel buffers and shapes ([`gfx`]), text from the font atlas
//! ([`font`]) and QOI pictures ([`qoi`]).

#![no_std]

extern crate alloc;

pub mod font;
pub mod gfx;
pub mod qoi;

pub use font::{Font, Fonts};
pub use gfx::{blend, mix, Canvas, Rect};
