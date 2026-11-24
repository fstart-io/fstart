// SPDX-License-Identifier: MIT OR Apache-2.0
//! All five DDR4 tradeoff functions from native/opt_tradeoff.c.
//! Scratch is borrowed, not static/global or allocated on the CAR stack.
use super::{Error, opt_param::MAX_OPT_POINTS};
pub const TRADEOFF_ROWS: usize = 5;

/// `meminit_effective_margin`: sum minus half the edge imbalance.
pub fn effective_margin(negative: u16, positive: u16) -> Result<u16, Error> {
    u16::try_from(
        (2 * (u32::from(negative) + u32::from(positive)) - u32::from(positive.abs_diff(negative)))
            / 2,
    )
    .map_err(|_| Error::InvalidPolicy)
}
fn dimensions(length: usize, x_dim: usize, x_min: usize) -> Result<(), Error> {
    if length == 0
        || length > MAX_OPT_POINTS
        || x_dim == 0
        || x_dim > MAX_OPT_POINTS
        || x_min > x_dim
    {
        return Err(Error::InvalidPolicy);
    }
    Ok(())
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum NeighbourBoundary {
    Skip,
    Clamp,
    RunningAverage,
}

fn neighbour(
    index: usize,
    dx: isize,
    dy: isize,
    length: usize,
    x_dim: usize,
    x_min: usize,
    boundary: NeighbourBoundary,
) -> Option<usize> {
    let clamp = boundary != NeighbourBoundary::Skip;
    let x = index % x_dim;
    let y = index / x_dim;
    let mut nx = x as isize + dx;
    let mut ny = y as isize + dy;
    if !clamp && (nx < 0 || nx >= x_dim as isize || ny < 0 || ny >= length.div_ceil(x_dim) as isize)
    {
        return None;
    }
    nx = nx.clamp(0, x_dim as isize - 1);
    ny = ny.clamp(0, length.div_ceil(x_dim) as isize - 1);
    if nx < x_min as isize && x < x_min {
        nx = x as isize;
    }
    let mut n = ny as usize * x_dim + nx as usize;
    if n >= length {
        if !clamp {
            return None;
        }
        if (dy != 0 || boundary == NeighbourBoundary::RunningAverage) && ny > 0 {
            ny -= 1;
        } else {
            nx = (length - 1) as isize % x_dim as isize;
        }
        n = ny as usize * x_dim + nx as usize;
    }
    (n < length).then_some(n)
}
/// `meminit_running_average_2d`; zero center weight is legal (RX EQ test0).
/// The caller supplies the 400-byte intermediate array.
pub fn running_average_2d(
    margins: &mut [u16],
    x_dim: usize,
    x_min: usize,
    center_scale: u8,
    scratch: &mut [u16; MAX_OPT_POINTS],
) -> Result<(), Error> {
    dimensions(margins.len(), x_dim, x_min)?;
    if margins.len() == 1 {
        return Ok(());
    }
    for i in 0..margins.len() {
        let mut sum = i64::from(margins[i]) * (i64::from(center_scale) - 1);
        for dx in -1..=1 {
            for dy in -1..=1 {
                let n = neighbour(
                    i,
                    dx,
                    dy,
                    margins.len(),
                    x_dim,
                    x_min,
                    NeighbourBoundary::RunningAverage,
                )
                .ok_or(Error::InvalidPolicy)?;
                sum += i64::from(margins[n]);
            }
        }
        scratch[i] =
            u16::try_from(sum / (8 + i64::from(center_scale))).map_err(|_| Error::InvalidPolicy)?;
    }
    margins.copy_from_slice(&scratch[..margins.len()]);
    Ok(())
}
/// `meminit_fill_2d_average`: only explicit unmeasured grid holes are filled.
/// Measured zero eyes remain measured zero, never inferred healthy eyes.
pub fn fill_2d_average(
    margins: &mut [u16],
    measured: &[bool],
    x_dim: usize,
    x_min: usize,
    center_scale: u8,
    scratch: &mut [u16; MAX_OPT_POINTS],
) -> Result<(), Error> {
    dimensions(margins.len(), x_dim, x_min)?;
    if measured.len() != margins.len() || !measured.iter().any(|x| *x) {
        return Err(Error::InvalidPolicy);
    }
    for (i, value) in margins.iter_mut().enumerate() {
        if !measured[i] {
            *value = 0;
        }
    }
    for i in 0..margins.len() {
        if measured[i] {
            continue;
        }
        let mut sum = i64::from(margins[i]) * (i64::from(center_scale) - 1);
        let mut count = 0;
        for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
            if let Some(n) = neighbour(
                i,
                dx,
                dy,
                margins.len(),
                x_dim,
                x_min,
                NeighbourBoundary::Skip,
            ) {
                sum += i64::from(margins[n]);
                count += 1;
            }
        }
        let denominator = count + i64::from(center_scale) - 1;
        if denominator <= 0 {
            return Err(Error::InvalidPolicy);
        }
        margins[i] = u16::try_from(sum / denominator).map_err(|_| Error::InvalidPolicy)?;
    }
    for i in 0..margins.len() {
        let mut gradient = 0;
        for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
            let n = neighbour(
                i,
                dx,
                dy,
                margins.len(),
                x_dim,
                x_min,
                NeighbourBoundary::Clamp,
            )
            .ok_or(Error::InvalidPolicy)?;
            gradient = gradient.max(margins[i].saturating_sub(margins[n]));
        }
        scratch[i] = margins[i] - gradient;
    }
    margins.copy_from_slice(&scratch[..margins.len()]);
    Ok(())
}
/// Source six-piece Heaviside approximation, integer hundredths.
pub fn linear_approx_trade_off(post_margin: u32, good_power_limit: u32) -> Result<u64, Error> {
    if good_power_limit == 0 {
        return Err(Error::InvalidPolicy);
    }
    let x = u64::from(post_margin) * 1000 / u64::from(good_power_limit);
    let segments = [
        (250, 10, 0),
        (750, 50, -100),
        (1250, 200, -1225),
        (2000, 50, 650),
        (4000, 10, 1450),
        (u64::MAX, 1, 1810),
    ];
    let (_, a, b) = segments.into_iter().find(|(max, _, _)| x <= *max).unwrap();
    Ok((x as i64 * a / 100 + b).max(0) as u64)
}

/// Major scorer arrays: 4000-byte post-filter plus 3600-byte results, and
/// 400-byte smoothing scratch. Caller must allocate in its CAR workspace.
pub struct TradeoffWorkspace {
    pub post: [[u32; MAX_OPT_POINTS]; TRADEOFF_ROWS],
    pub widths: [[u16; MAX_OPT_POINTS]; TRADEOFF_ROWS],
    pub scores: [u64; MAX_OPT_POINTS],
    pub scratch: [u16; MAX_OPT_POINTS],
}
impl TradeoffWorkspace {
    pub const ZERO: Self = Self {
        post: [[0; MAX_OPT_POINTS]; TRADEOFF_ROWS],
        widths: [[0; MAX_OPT_POINTS]; TRADEOFF_ROWS],
        scores: [0; MAX_OPT_POINTS],
        scratch: [0; MAX_OPT_POINTS],
    };
}
#[derive(Debug, Clone, Copy)]
pub struct TradeoffConfig {
    pub scale: [u8; 5],
    pub upm_optimize: [bool; 5],
    pub square: bool,
    pub average: u8,
    pub include_ends: bool,
    pub center_scale: u8,
    pub good_power_limit: [u16; 5],
    /// 0=first UPM-satisfying point; 1..4=power row; 5..99=score percentage;
    /// 100=margin-only maximum, optionally with the linear approximation.
    pub optimization_mode: u8,
    pub guard_band: i16,
    pub linear: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradeoffResult {
    pub best: usize,
    pub guard_band: i16,
    pub max_score: u64,
    pub max_post: [u32; 5],
    pub min_post: [u32; 5],
}

/// `meminit_find_optimal_trade_off`. Source smoothing/scales/UPM/power
/// preference, strict '>' deterministic ties and signed guardbands. Invalid
/// dimensions, zero active scales/linear divisors or arithmetic reject.
pub fn find_optimal_trade_off(
    rows: &[[u16; MAX_OPT_POINTS]; 5],
    length: usize,
    cfg: &TradeoffConfig,
    work: &mut TradeoffWorkspace,
) -> Result<TradeoffResult, Error> {
    if length == 0
        || length > MAX_OPT_POINTS
        || !cfg.scale.iter().any(|s| *s != 0)
        || cfg.optimization_mode > 100
    {
        return Err(Error::InvalidPolicy);
    }
    let ave = usize::from(cfg.average.max(1));
    let divisor = ave + usize::from(cfg.center_scale);
    let divisor = divisor
        .checked_sub(1)
        .filter(|x| *x != 0)
        .ok_or(Error::InvalidPolicy)? as u32;
    let half = ave / 2;
    let mut max_post = [1u32; 5];
    let mut min_post = [u32::MAX; 5];
    let mut limits = [0u32; 5];
    let mut result = TradeoffResult {
        best: 0,
        guard_band: 0,
        max_score: 0,
        max_post: [0; 5],
        min_post: [0; 5],
    };
    let active = cfg.scale.iter().filter(|s| **s != 0).count() as u32;
    let order = if cfg.square { 2 * active - 1 } else { active };
    for row in 0..5 {
        limits[row] = u32::from(cfg.good_power_limit[row])
            .checked_mul(divisor)
            .and_then(|x| x.checked_mul(u32::from(cfg.scale[row])))
            .ok_or(Error::InvalidPolicy)?;
        if cfg.linear && cfg.optimization_mode == 100 && cfg.scale[row] != 0 && limits[row] == 0 {
            return Err(Error::InvalidPolicy);
        }
        if cfg.upm_optimize[row] && cfg.scale[row] == 0 {
            return Err(Error::InvalidPolicy);
        }
        for x in 0..length {
            work.widths[row][x] = 0;
            work.post[row][x] = 1;
            if cfg.scale[row] == 0 {
                min_post[row] = 1;
                continue;
            }
            let mut sum = 0u32;
            for off in 0..ave {
                let effective = x as isize + off as isize - half as isize;
                let idx = effective.clamp(0, length as isize - 1) as usize;
                let weight = if effective == x as isize {
                    u32::from(cfg.center_scale)
                } else {
                    1
                };
                sum = sum
                    .checked_add(u32::from(rows[row][idx]) * weight)
                    .ok_or(Error::InvalidPolicy)?;
            }
            work.widths[row][x] = u16::try_from(sum / divisor).map_err(|_| Error::InvalidPolicy)?;
            let scaled = sum
                .checked_mul(u32::from(cfg.scale[row]))
                .ok_or(Error::InvalidPolicy)?;
            work.post[row][x] = scaled;
            max_post[row] = max_post[row].max(scaled);
            min_post[row] = min_post[row].min(scaled);
        }
        if cfg.scale[row] != 0 {
            result.max_post[row] = max_post[row] / divisor;
            result.min_post[row] = min_post[row] / divisor;
        }
    }
    let mut sorted = max_post;
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    let mut bits = 11;
    for (i, &v) in sorted.iter().enumerate() {
        let v = u64::from(v);
        let v = if cfg.square && i < 4 { v * v } else { v };
        bits += 64 - v.leading_zeros();
    }
    if bits > 64 {
        let shift = (bits - 64).div_ceil(order);
        if shift >= 32 {
            return Err(Error::InvalidPolicy);
        }
        let adder = 1u64 << (shift - 1);
        for row in 0..5 {
            max_post[row] = ((u64::from(max_post[row]) + adder) >> shift) as u32;
            limits[row] = ((u64::from(limits[row]) + adder) >> shift) as u32;
            for x in 0..length {
                work.post[row][x] = ((u64::from(work.post[row][x]) + adder) >> shift) as u32;
            }
        }
    }
    if cfg.square {
        for v in &mut max_post {
            *v = v.checked_mul(*v).ok_or(Error::InvalidPolicy)?;
        }
    }
    let start = usize::from(!cfg.include_ends && length > ave && half > 0);
    let stop = length - start;
    let mut best = start;
    let mut power = None;
    for x in start..stop {
        let mut score = 0u128;
        let mut minimum = u32::MAX;
        let mut good = true;
        for row in 0..5 {
            if cfg.scale[row] == 0 {
                continue;
            }
            if cfg.upm_optimize[row] && work.post[row][x] > limits[row] {
                work.post[row][x] = limits[row];
                work.widths[row][x] =
                    u16::try_from(work.post[row][x] / divisor / u32::from(cfg.scale[row]))
                        .map_err(|_| Error::InvalidPolicy)?;
            }
            let post = work.post[row][x];
            minimum = minimum.min(post);
            good &= post >= limits[row];
            if cfg.linear && cfg.optimization_mode == 100 {
                score += u128::from(linear_approx_trade_off(post, limits[row])?);
            } else {
                let mut product = 1u128;
                for other in 0..5 {
                    if other != row && cfg.scale[other] != 0 {
                        product = product
                            .checked_mul(u128::from(max_post[other]))
                            .ok_or(Error::InvalidPolicy)?;
                    }
                }
                score = score
                    .checked_add(
                        product
                            .checked_mul(u128::from(post))
                            .ok_or(Error::InvalidPolicy)?,
                    )
                    .ok_or(Error::InvalidPolicy)?;
            }
        }
        if !(cfg.linear && cfg.optimization_mode == 100) {
            score = score
                .checked_add(
                    u128::from(minimum)
                        .checked_pow(order)
                        .ok_or(Error::InvalidPolicy)?,
                )
                .ok_or(Error::InvalidPolicy)?;
        }
        let score = u64::try_from(score).map_err(|_| Error::InvalidPolicy)?;
        work.scores[x] = score;
        if score > result.max_score {
            result.max_score = score;
            best = x;
        }
        if good {
            if let Some(previous) = power {
                let p = usize::from(cfg.optimization_mode);
                if p > 0 && p < 5 && work.post[p][x] > work.post[p][previous] {
                    power = Some(x);
                }
            } else {
                power = Some(x);
            }
        }
    }
    if let Some(p) = power {
        if cfg.optimization_mode < 5 {
            best = p;
        } else if cfg.optimization_mode < 100 && result.max_score != 0 {
            for x in p..stop {
                if u128::from(work.scores[x]) * 100 / u128::from(result.max_score)
                    >= u128::from(cfg.optimization_mode)
                {
                    best = x;
                    break;
                }
            }
        }
    }
    result.best = best;
    if cfg.guard_band != 0 {
        let mut off =
            (best as i32 + i32::from(cfg.guard_band)).clamp(start as i32, stop as i32 - 1) as usize;
        let step = if cfg.guard_band < 0 { 1 } else { -1 };
        let threshold = u128::from(work.scores[best]) * 90 / 100;
        while off != best && u128::from(work.scores[off]) <= threshold {
            off = (off as isize + step) as usize;
        }
        result.guard_band = off as i16 - best as i16;
    }
    Ok(result)
}
