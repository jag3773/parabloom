//! Plain data types shared by extraction and layout.
//!
//! Coordinates are stored as `f64` widened from MuPDF's `f32`, so arithmetic
//! matches the Python reference implementation (which does its arithmetic in
//! doubles on the same `f32`-valued numbers).

use serde::{Deserialize, Serialize};

/// One text line: its bounding box and its text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub text: String,
}

/// A text block as MuPDF segments it: a bounding box and its lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub bbox: [f64; 4],
    pub lines: Vec<Line>,
}

/// A paragraph is an ordered run of lines on one page.
pub type Paragraph = Vec<Line>;

/// An axis-aligned rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}
