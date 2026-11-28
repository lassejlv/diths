//! Exact acceleration for nearest-palette-color lookups.
//!
//! [`Palette::nearest_index_from_linear`] and [`Palette::nearest_index_from_srgb`] scan every
//! palette entry for every pixel, and the Oklab scan also converts the sample to Oklab first. A
//! [`Matcher`] returns exactly the index those scans return, but looks the sample up in a coarse
//! 3D grid over the working color space first. Each grid cell stores the palette entries that can
//! win somewhere inside it: one entry means the answer is known without any color math, a short
//! list means only those entries are measured, and anything longer falls back to the full scan.
//!
//! The lists are built with conservative bounds (see [`Matcher::fill_cell`]), so an entry is only
//! left out of a cell when the `f32` scan could not pick it for any sample in that cell. Results
//! are therefore identical to the scan on every platform; only the amount of work changes.

use std::fmt;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::color::{ColorSpace, LinearRgb};
use crate::detmath;
use crate::palette::Palette;

/// Grid cells per axis.
const GRID: usize = 32;
/// A sample's cell along one axis is looked up by the top bits of its `f32` pattern: the sign, the
/// exponent and the first five mantissa bits, 32 keys per octave.
const KEY_SHIFT: u32 = 18;
/// Number of keys: every `f32` bit pattern has one.
const KEY_COUNT: usize = 1 << (32 - KEY_SHIFT);
/// The key of `1.0`. Keys above it, and every negative key, hold [`OUTSIDE`].
const KEY_ONE: usize = (1.0f32.to_bits() >> KEY_SHIFT) as usize;
/// Marks keys of values outside `0.0..=1.0` (including `-0.0`, which the full scan handles).
const OUTSIDE: u8 = 0x80;
/// Longest candidate list stored in a cell (seven `u8` indices beside the length byte).
const MAX_SHORTLIST: usize = 7;
/// Candidate sets larger than this are not pruned pairwise; the cell falls back to a full scan.
const MAX_PRUNE_SET: usize = 16;
const UNSET: u64 = 0;
const FULL_SCAN: u64 = 0xFF;

/// Bound on the absolute difference, per Oklab coordinate, between [`LinearRgb::to_oklab`] in
/// `f32` and the exact value of the same formula, for samples in `[0, 1]`. The measured worst
/// case is below `1.5e-5` (see the tests); this leaves a wide safety factor.
const OKLAB_ERROR: f64 = 1e-4;
/// Bound on the relative rounding error of an `f32` squared distance (a few ulps), again with a
/// wide safety factor.
const DISTANCE_ERROR: f64 = 1e-5;
/// `fast_cbrt_nonneg` returns zero at or below this input.
const CBRT_ZERO_BELOW: f64 = 1e-12;

/// Same coefficients as [`LinearRgb::to_oklab`].
const LMS_FROM_LINEAR: [[f32; 3]; 3] = [
    [0.412_221_47, 0.536_332_55, 0.051_445_995],
    [0.211_903_5, 0.680_699_5, 0.107_396_96],
    [0.088_302_46, 0.281_718_84, 0.629_978_7],
];
const OKLAB_FROM_LMS: [[f32; 3]; 3] = [
    [0.210_454_26, 0.793_617_8, -0.004_072_047],
    [1.977_998_5, -2.428_592_2, 0.450_593_7],
    [0.025_904_037, 0.782_771_77, -0.808_675_77],
];

/// How one axis of the working space is cut into [`GRID`] cells.
#[derive(Debug)]
struct Axis {
    /// Cell of every key, or [`OUTSIDE`].
    cell_of_key: [u8; KEY_COUNT],
    /// `edges[c]..=edges[c + 1]` contains every sample of cell `c`.
    edges: [f64; GRID + 1],
}

impl Axis {
    /// Cut the axis so that cells are evenly sized after `spacing` (monotonic, `0..=1` to `0..=1`).
    fn new(spacing: impl Fn(f64) -> f64) -> Self {
        let mut cell_of_key = [OUTSIDE; KEY_COUNT];
        let mut edges = [1.0; GRID + 1];
        edges[0] = 0.0;
        let mut previous = 0;
        for (key, slot) in cell_of_key[..=KEY_ONE].iter_mut().enumerate() {
            let lowest = f64::from(f32::from_bits((key as u32) << KEY_SHIFT));
            let cell = ((spacing(lowest) * GRID as f64) as usize).min(GRID - 1);
            for edge in &mut edges[previous + 1..=cell] {
                *edge = lowest;
            }
            previous = cell;
            *slot = cell as u8;
        }
        Self { cell_of_key, edges }
    }
}

/// Cells evenly sized in the cube root, for linear-light samples: the Oklab scan measures
/// distances after a cube root, and palettes (and photographs) crowd the dark end of linear light.
static CBRT_AXIS: LazyLock<Axis> = LazyLock::new(|| Axis::new(detmath::cbrt));
/// Evenly sized cells for gamma-encoded sRGB samples.
static UNIFORM_AXIS: LazyLock<Axis> = LazyLock::new(|| Axis::new(|v| v));

/// Lazily filled candidate lists for one palette in one color space.
pub(crate) struct NearestGrid {
    cells: Vec<AtomicU64>,
}

impl NearestGrid {
    pub(crate) fn new() -> Self {
        Self {
            cells: (0..GRID * GRID * GRID)
                .map(|_| AtomicU64::new(UNSET))
                .collect(),
        }
    }
}

impl fmt::Debug for NearestGrid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NearestGrid").finish_non_exhaustive()
    }
}

/// Nearest-palette-color lookups for one palette and color space.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Matcher<'a> {
    palette: &'a Palette,
    space: ColorSpace,
    grid: Option<&'a NearestGrid>,
    axis: &'static Axis,
}

impl<'a> Matcher<'a> {
    /// Largest palette whose indices fit the `u8` cell encoding; larger ones always scan.
    pub(crate) const MAX_PALETTE: usize = 256;

    pub(crate) fn new(
        palette: &'a Palette,
        space: ColorSpace,
        grid: Option<&'a NearestGrid>,
    ) -> Self {
        Self {
            palette,
            space,
            grid,
            axis: match space {
                ColorSpace::Oklab | ColorSpace::Linear => &CBRT_AXIS,
                ColorSpace::Srgb => &UNIFORM_AXIS,
            },
        }
    }

    /// Index of the palette color closest to `sample`, given in the working space of the color
    /// space: linear RGB for Oklab and linear, normalized sRGB for sRGB. Identical to
    /// [`Palette::nearest_index_from_linear`] / [`Palette::nearest_index_from_srgb`].
    #[inline]
    pub(crate) fn nearest(&self, sample: [f32; 3]) -> usize {
        if let Some(grid) = self.grid {
            let [cx, cy, cz] =
                sample.map(|v| self.axis.cell_of_key[(v.to_bits() >> KEY_SHIFT) as usize]);
            // `<= 1.0` rejects the values that share 1.0's key, and NaN.
            if (cx | cy | cz) & OUTSIDE == 0
                && sample[0] <= 1.0
                && sample[1] <= 1.0
                && sample[2] <= 1.0
            {
                let index = (usize::from(cx) | usize::from(cy) << 5 | usize::from(cz) << 10)
                    & (GRID * GRID * GRID - 1);
                let cell = grid.cells[index].load(Ordering::Relaxed);
                if cell & 0xFF == 1 {
                    return ((cell >> 8) & 0xFF) as usize;
                }
                return self.nearest_in_cell(grid, sample, index, cell);
            }
        }
        self.scan_all(sample)
    }

    /// The rest of [`Self::nearest`] once the cell is known, kept out of line so the common
    /// single-candidate case stays small enough to inline.
    #[inline(never)]
    fn nearest_in_cell(
        &self,
        grid: &NearestGrid,
        sample: [f32; 3],
        index: usize,
        cell: u64,
    ) -> usize {
        let cell = if cell == UNSET {
            self.fill_cell(grid, index)
        } else {
            cell
        };
        match cell & 0xFF {
            1 => ((cell >> 8) & 0xFF) as usize,
            FULL_SCAN => self.scan_all(sample),
            _ => self.scan_shortlist(sample, cell),
        }
    }

    /// The reference full scan.
    #[inline]
    fn scan_all(&self, sample: [f32; 3]) -> usize {
        match self.space {
            ColorSpace::Oklab | ColorSpace::Linear => self
                .palette
                .nearest_index_from_linear(LinearRgb::from_array(sample), self.space),
            ColorSpace::Srgb => self.palette.nearest_index_from_srgb(sample),
        }
    }

    /// The reference scan restricted to a cell's candidates, in ascending index order, with the
    /// same distance expressions and the same first-minimum-wins rule.
    #[inline]
    fn scan_shortlist(&self, sample: [f32; 3], cell: u64) -> usize {
        let len = (cell & 0xFF) as usize;
        let mut best_idx = 0;
        let mut best_dist = f32::INFINITY;
        match self.space {
            ColorSpace::Oklab => {
                let target = LinearRgb::from_array(sample).to_oklab();
                let colors = self.palette.oklab_colors();
                for slot in 1..=len {
                    let idx = ((cell >> (8 * slot)) & 0xFF) as usize;
                    let d = target.dist_sq(colors[idx]);
                    if d < best_dist {
                        best_dist = d;
                        best_idx = idx;
                    }
                }
            }
            ColorSpace::Linear => {
                let target = LinearRgb::from_array(sample);
                let colors = self.palette.linear_colors();
                for slot in 1..=len {
                    let idx = ((cell >> (8 * slot)) & 0xFF) as usize;
                    let d = target.dist_sq(colors[idx]);
                    if d < best_dist {
                        best_dist = d;
                        best_idx = idx;
                    }
                }
            }
            ColorSpace::Srgb => {
                let colors = self.palette.srgb_colors();
                for slot in 1..=len {
                    let idx = ((cell >> (8 * slot)) & 0xFF) as usize;
                    let cand = colors[idx];
                    let dr = sample[0] - cand[0];
                    let dg = sample[1] - cand[1];
                    let db = sample[2] - cand[2];
                    let d = dr * dr + dg * dg + db * db;
                    if d < best_dist {
                        best_dist = d;
                        best_idx = idx;
                    }
                }
            }
        }
        best_idx
    }

    /// Work out which palette entries the full scan can return for samples in cell `index`.
    ///
    /// For two entries `k` and `j` the scan prefers `k` wherever `d_k - d_j < 0`, and
    /// `d_k(t) - d_j(t) = |c_k|^2 - |c_j|^2 - 2 t . (c_k - c_j)` is linear in the matching-space
    /// position `t`. For linear and sRGB `t` is the sample, so the maximum of that difference over
    /// the cell is exact. For Oklab `t = M q` where `q` holds the cube roots of the three LMS
    /// responses, each a positive mix of the sample channels. The cube root is concave, so it
    /// lies below its tangent at the cell center and above its chord across the cell; replacing
    /// each cube root by whichever of the two over-estimates the difference gives a bound that is
    /// linear in the sample, and its maximum over the cell is again exact.
    ///
    /// Entry `k` rules `j` out when that maximum is below zero by more than everything `f32`
    /// rounding could contribute: [`OKLAB_ERROR`] on `t`, the snap to zero of tiny cube roots, and
    /// [`DISTANCE_ERROR`] on each distance. An entry that the scan picks for some sample in the
    /// cell is never ruled out, whichever order the tests run in, so the surviving list always
    /// contains the scan's answer.
    #[cold]
    #[inline(never)]
    fn fill_cell(&self, grid: &NearestGrid, index: usize) -> u64 {
        let cells = [index % GRID, index / GRID % GRID, index / (GRID * GRID)];
        let lo = cells.map(|c| self.axis.edges[c]);
        let hi = cells.map(|c| self.axis.edges[c + 1]);
        let center = [0, 1, 2].map(|axis| f64::midpoint(lo[axis], hi[axis]));

        let points: Vec<[f64; 3]> = match self.space {
            ColorSpace::Oklab => self
                .palette
                .oklab_colors()
                .iter()
                .map(|c| [f64::from(c.l), f64::from(c.a), f64::from(c.b)])
                .collect(),
            ColorSpace::Linear => self
                .palette
                .linear_colors()
                .iter()
                .map(|c| c.to_array().map(f64::from))
                .collect(),
            ColorSpace::Srgb => self
                .palette
                .srgb_colors()
                .iter()
                .map(|c| c.map(f64::from))
                .collect(),
        };
        let oklab = (self.space == ColorSpace::Oklab).then(|| OklabCell::new(lo, hi, center));

        // Bounding box of t over the cell (loose for Oklab; it only scales the rounding margin)
        // and t at the cell center.
        let (t_lo, t_hi, t_center) = match &oklab {
            Some(cell) => cell.matching_space_box(),
            None => (lo, hi, center),
        };
        let farthest: Vec<f64> = points
            .iter()
            .map(|p| {
                (0..3)
                    .map(|axis| {
                        let a = t_lo[axis] - p[axis];
                        let b = t_hi[axis] - p[axis];
                        (a * a).max(b * b)
                    })
                    .sum()
            })
            .collect();
        let norms: Vec<f64> = points
            .iter()
            .map(|p| p[0] * p[0] + p[1] * p[1] + p[2] * p[2])
            .collect();

        // True when entry k is strictly closer than entry j for every sample in the cell.
        let rules_out = |k: usize, j: usize| {
            let diff = [
                points[k][0] - points[j][0],
                points[k][1] - points[j][1],
                points[k][2] - points[j][2],
            ];
            // d_k - d_j <= constant + slope . sample, plus a rounding allowance.
            let (constant, slope, rounding) = match &oklab {
                Some(cell) => cell.linear_bound(diff),
                None => (0.0, diff.map(|d| -2.0 * d), 0.0),
            };
            let mut max_gap = norms[k] - norms[j] + constant + rounding;
            for axis in 0..3 {
                max_gap += (slope[axis] * lo[axis]).max(slope[axis] * hi[axis]);
            }
            max_gap < -DISTANCE_ERROR * (farthest[k] + farthest[j]) - 1e-12
        };

        let distance_to_center = |p: &[f64; 3]| {
            (0..3)
                .map(|axis| (t_center[axis] - p[axis]) * (t_center[axis] - p[axis]))
                .sum::<f64>()
        };
        let anchor = (0..points.len())
            .min_by(|&a, &b| {
                distance_to_center(&points[a]).total_cmp(&distance_to_center(&points[b]))
            })
            .unwrap_or(0);
        let mut candidates: Vec<usize> = (0..points.len())
            .filter(|&j| j == anchor || !rules_out(anchor, j))
            .collect();
        if candidates.len() > 2 && candidates.len() <= MAX_PRUNE_SET {
            let mut i = 0;
            while i < candidates.len() {
                let j = candidates[i];
                if candidates.iter().any(|&k| k != j && rules_out(k, j)) {
                    candidates.remove(i);
                } else {
                    i += 1;
                }
            }
        }

        let cell = if candidates.len() > MAX_SHORTLIST {
            FULL_SCAN
        } else {
            candidates
                .iter()
                .enumerate()
                .fold(candidates.len() as u64, |acc, (slot, &idx)| {
                    acc | ((idx as u64) << (8 * (slot + 1)))
                })
        };
        grid.cells[index].store(cell, Ordering::Relaxed);
        cell
    }
}

/// Per-cell data for bounding Oklab distance differences linearly in the sample.
struct OklabCell {
    /// For each LMS response `u`: the cube root is at most `tangent.0 + tangent.1 * u`.
    tangent: [(f64, f64); 3],
    /// For each LMS response `u`: the cube root is at least `chord.0 + chord.1 * u`.
    chord: [(f64, f64); 3],
    /// Range of each cube root over the cell.
    q_lo: [f64; 3],
    q_hi: [f64; 3],
    q_center: [f64; 3],
    /// How far `fast_cbrt_nonneg` can fall below the exact cube root by snapping to zero.
    snap: [f64; 3],
}

impl OklabCell {
    fn new(lo: [f64; 3], hi: [f64; 3], center: [f64; 3]) -> Self {
        let mut cell = Self {
            tangent: [(0.0, 0.0); 3],
            chord: [(0.0, 0.0); 3],
            q_lo: [0.0; 3],
            q_hi: [0.0; 3],
            q_center: [0.0; 3],
            snap: [0.0; 3],
        };
        for (axis, row) in LMS_FROM_LINEAR.iter().enumerate() {
            let response = |corner: [f64; 3]| {
                f64::from(row[0]) * corner[0]
                    + f64::from(row[1]) * corner[1]
                    + f64::from(row[2]) * corner[2]
            };
            let (u_lo, u_hi, u_center) = (response(lo), response(hi), response(center));
            let (q_lo, q_hi, q_center) = (
                detmath::cbrt(u_lo),
                detmath::cbrt(u_hi),
                detmath::cbrt(u_center),
            );
            let tangent_slope = 1.0 / (3.0 * q_center * q_center);
            let chord_slope = (q_hi - q_lo) / (u_hi - u_lo);
            cell.tangent[axis] = (q_center - tangent_slope * u_center, tangent_slope);
            cell.chord[axis] = (q_lo - chord_slope * u_lo, chord_slope);
            cell.q_lo[axis] = q_lo;
            cell.q_hi[axis] = q_hi;
            cell.q_center[axis] = q_center;
            // Responses this small (with room for their own f32 rounding) come back as zero.
            if u_lo <= 2.0 * CBRT_ZERO_BELOW {
                cell.snap[axis] = detmath::cbrt(2.0 * CBRT_ZERO_BELOW);
            }
        }
        cell
    }

    /// A box containing every Oklab position the `f32` conversion can produce in this cell, and
    /// the exact position of the cell center.
    fn matching_space_box(&self) -> ([f64; 3], [f64; 3], [f64; 3]) {
        let mut t_lo = [0.0; 3];
        let mut t_hi = [0.0; 3];
        let mut t_center = [0.0; 3];
        for (axis, row) in OKLAB_FROM_LMS.iter().enumerate() {
            t_lo[axis] = -OKLAB_ERROR;
            t_hi[axis] = OKLAB_ERROR;
            for (k, &coefficient) in row.iter().enumerate() {
                let coefficient = f64::from(coefficient);
                let a = coefficient * (self.q_lo[k] - self.snap[k]);
                let b = coefficient * self.q_hi[k];
                t_lo[axis] += a.min(b);
                t_hi[axis] += a.max(b);
                t_center[axis] += coefficient * self.q_center[k];
            }
        }
        (t_lo, t_hi, t_center)
    }

    /// For `diff = c_k - c_j`: `(constant, slope, rounding)` such that, for every sample in the
    /// cell, the scan's `-2 t . diff` is at most `constant + slope . sample + rounding`.
    fn linear_bound(&self, diff: [f64; 3]) -> (f64, [f64; 3], f64) {
        let mut constant = 0.0;
        let mut slope = [0.0; 3];
        let mut rounding = 2.0 * OKLAB_ERROR * (diff[0].abs() + diff[1].abs() + diff[2].abs());
        for (k, row) in LMS_FROM_LINEAR.iter().enumerate() {
            // -2 t . diff = sum over k of weight * cbrt(response_k).
            let weight = -2.0
                * (f64::from(OKLAB_FROM_LMS[0][k]) * diff[0]
                    + f64::from(OKLAB_FROM_LMS[1][k]) * diff[1]
                    + f64::from(OKLAB_FROM_LMS[2][k]) * diff[2]);
            let (offset, per_response) = if weight >= 0.0 {
                self.tangent[k]
            } else {
                // A cube root snapped to zero sits below the chord by up to `snap`.
                rounding += -weight * self.snap[k];
                self.chord[k]
            };
            constant += weight * offset;
            for axis in 0..3 {
                slope[axis] += weight * per_response * f64::from(row[axis]);
            }
        }
        (constant, slope, rounding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Rgb;
    use crate::palette::{SplitMix64, builtin_palette_ids};

    fn reference(palette: &Palette, space: ColorSpace, sample: [f32; 3]) -> usize {
        match space {
            ColorSpace::Oklab | ColorSpace::Linear => {
                palette.nearest_index_from_linear(LinearRgb::from_array(sample), space)
            }
            ColorSpace::Srgb => palette.nearest_index_from_srgb(sample),
        }
    }

    fn test_palettes() -> Vec<Palette> {
        let mut palettes: Vec<Palette> = builtin_palette_ids()
            .into_iter()
            .map(|id| Palette::by_name(id).unwrap())
            .collect();
        let mut rng = SplitMix64::new(7);
        for count in [2, 3, 5, 17, 64, 256, 300] {
            let colors = (0..count)
                .map(|_| Rgb::from_u32(rng.next_u64() as u32))
                .collect();
            palettes.push(Palette::new(format!("random-{count}"), "", colors).unwrap());
        }
        // Near-duplicate and duplicate entries make ties and razor-thin cells.
        palettes.push(
            Palette::new(
                "ties",
                "",
                vec![
                    Rgb::new(10, 10, 10),
                    Rgb::new(10, 10, 11),
                    Rgb::new(200, 30, 30),
                    Rgb::new(200, 30, 30),
                    Rgb::new(11, 10, 10),
                ],
            )
            .unwrap(),
        );
        palettes
    }

    fn assert_matches(palette: &Palette, space: ColorSpace, sample: [f32; 3]) {
        assert_eq!(
            palette.matcher(space).nearest(sample),
            reference(palette, space, sample),
            "palette {} in {space:?} at {sample:?}",
            palette.name
        );
    }

    #[test]
    fn matcher_returns_the_full_scan_index_for_random_and_edge_samples() {
        let edges = [
            0.0,
            -0.0,
            1.0,
            f32::MIN_POSITIVE,
            1e-12,
            1e-5,
            0.5,
            1.0 - f32::EPSILON,
            1.0 + f32::EPSILON,
            -1e-9,
            2.0,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        for palette in test_palettes() {
            for space in [ColorSpace::Oklab, ColorSpace::Linear, ColorSpace::Srgb] {
                let mut rng = SplitMix64::new(0xD17B ^ palette.len() as u64);
                for _ in 0..60_000 {
                    assert_matches(
                        &palette,
                        space,
                        [rng.next_f32(), rng.next_f32(), rng.next_f32()],
                    );
                }
                // Dark samples, where the cube root is steepest.
                for _ in 0..20_000 {
                    let scale = 0.1f32.powi((rng.next_u64() % 6) as i32 + 1);
                    assert_matches(
                        &palette,
                        space,
                        [
                            rng.next_f32() * scale,
                            rng.next_f32() * scale,
                            rng.next_f32() * scale,
                        ],
                    );
                }
                for &r in &edges {
                    for &g in &edges {
                        for &b in &edges {
                            assert_matches(&palette, space, [r, g, b]);
                        }
                    }
                }
                // Cell boundaries and their neighbouring floats.
                for cell in 0..=GRID {
                    let v = cell as f32 / GRID as f32;
                    for edge in [v.next_down(), v, v.next_up()] {
                        let other = rng.next_f32();
                        assert_matches(&palette, space, [edge, other, edge]);
                        assert_matches(&palette, space, [other, edge, other]);
                        assert_matches(&palette, space, [edge, edge, edge]);
                    }
                }
            }
        }
    }

    #[test]
    fn matcher_returns_the_full_scan_index_right_at_decision_boundaries() {
        for palette in test_palettes() {
            for space in [ColorSpace::Oklab, ColorSpace::Linear, ColorSpace::Srgb] {
                let mut rng = SplitMix64::new(0xB15E ^ palette.len() as u64);
                for _ in 0..400 {
                    let mut a = [rng.next_f32(), rng.next_f32(), rng.next_f32()];
                    let mut b = [rng.next_f32(), rng.next_f32(), rng.next_f32()];
                    let index_a = reference(&palette, space, a);
                    if index_a == reference(&palette, space, b) {
                        continue;
                    }
                    // Bisect to the pair of adjacent samples where the scan's answer flips.
                    for _ in 0..40 {
                        let mid = [
                            f32::midpoint(a[0], b[0]),
                            f32::midpoint(a[1], b[1]),
                            f32::midpoint(a[2], b[2]),
                        ];
                        if reference(&palette, space, mid) == index_a {
                            a = mid;
                        } else {
                            b = mid;
                        }
                    }
                    for base in [a, b] {
                        assert_matches(&palette, space, base);
                        for axis in 0..3 {
                            let mut up = base;
                            up[axis] = up[axis].next_up().min(1.0);
                            assert_matches(&palette, space, up);
                            let mut down = base;
                            down[axis] = down[axis].next_down().max(0.0);
                            assert_matches(&palette, space, down);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn oklab_conversion_stays_within_the_error_bound_the_grid_assumes() {
        let exact = |sample: [f32; 3]| {
            let q: Vec<f64> = LMS_FROM_LINEAR
                .iter()
                .map(|row| {
                    let response = f64::from(row[0]) * f64::from(sample[0])
                        + f64::from(row[1]) * f64::from(sample[1])
                        + f64::from(row[2]) * f64::from(sample[2]);
                    response.cbrt()
                })
                .collect();
            OKLAB_FROM_LMS.map(|row| {
                f64::from(row[0]) * q[0] + f64::from(row[1]) * q[1] + f64::from(row[2]) * q[2]
            })
        };
        let mut rng = SplitMix64::new(99);
        let mut worst = 0.0f64;
        for i in 0..2_000_000 {
            // Bias a third of the samples toward black, where the relative error is largest.
            let scale = if i % 3 == 0 {
                0.1f32.powi((rng.next_u64() % 5) as i32)
            } else {
                1.0
            };
            let sample = [
                rng.next_f32() * scale,
                rng.next_f32() * scale,
                rng.next_f32() * scale,
            ];
            // Below the cutoff the model and the f32 code both snap to zero; skip the seam.
            if sample.iter().any(|&v| v < 1e-9) {
                continue;
            }
            let got = LinearRgb::from_array(sample).to_oklab();
            let want = exact(sample);
            for (g, w) in [got.l, got.a, got.b].into_iter().zip(want) {
                worst = worst.max((f64::from(g) - w).abs());
            }
        }
        assert!(
            worst < OKLAB_ERROR / 4.0,
            "f32 Oklab error {worst:e} is too close to the assumed bound {OKLAB_ERROR:e}"
        );
    }
}
