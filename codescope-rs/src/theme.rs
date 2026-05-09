//! Color tokens lifted from CodeScope's
//! `src/CodeScope.App/Styles/DesignTokens.xaml`. Same hex values, same
//! names — when the visual designers update the C# tokens we update
//! these and the two builds stay in lock-step.
//!
//! Philosophy (DESIGN.md §2):
//! * Binary surface — pure black canvas + pure white ink.
//! * One accent — Framer Blue. Used for focus rings, the active tab
//!   underline, and links. **No** secondary accent.
//! * Frosted glass via white-on-black alpha tiers (10 / 20 / 50).
//! * Pill geometry on every interactive CTA (radius 40+, never square).

use gpui::{Hsla, rgb, rgba};

// ─── Binary ink + canvas ────────────────────────────────────────────

pub fn canvas() -> Hsla { rgb(0x000000).into() }
pub fn near_black() -> Hsla { rgb(0x090909).into() }
pub fn ink() -> Hsla { rgb(0xffffff).into() }
pub fn ink_muted() -> Hsla { rgb(0xa6a6a6).into() }
pub fn ink_dim() -> Hsla { rgba(0xffffff99).into() }   // 0.60 alpha
pub fn ink_ghost() -> Hsla { rgba(0xffffff66).into() } // 0.40 alpha
pub fn divider() -> Hsla { rgba(0xffffff22).into() }

// ─── Framer Blue (the only accent) ──────────────────────────────────

#[allow(dead_code)]
pub fn accent() -> Hsla { rgb(0x0099ff).into() }
#[allow(dead_code)]
pub fn accent_glow() -> Hsla { rgba(0x0099ff26).into() }     // 0.15
#[allow(dead_code)]
pub fn accent_glow_soft() -> Hsla { rgba(0x0099ff14).into() } // 0.08

// ─── Frosted glass (white over black) ──────────────────────────────

pub fn frost_10() -> Hsla { rgba(0xffffff1a).into() } // button surface
pub fn frost_20() -> Hsla { rgba(0xffffff33).into() } // hover
#[allow(dead_code)]
pub fn frost_50() -> Hsla { rgba(0xffffff80).into() } // emphasis hover

// ─── Surfaces (Overview tokens — exact values from C# build) ────────

#[allow(dead_code)]
pub fn surface_elev() -> Hsla { rgb(0x141414).into() }
#[allow(dead_code)]
pub fn surface_border() -> Hsla { rgb(0x1f1f1f).into() }

// ─── Status dots ────────────────────────────────────────────────────

#[allow(dead_code)]
pub fn status_running() -> Hsla { rgb(0x22c55e).into() } // emerald
#[allow(dead_code)]
pub fn status_idle() -> Hsla { ink_muted() }
#[allow(dead_code)]
pub fn status_error() -> Hsla { rgb(0xef4444).into() } // red
