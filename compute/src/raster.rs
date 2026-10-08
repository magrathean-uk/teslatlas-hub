// SPDX-License-Identifier: Apache-2.0

use std::sync::atomic::{AtomicBool, Ordering};

use crate::Error;

const TILE_SIZE: u32 = 512;
const MIN_ZOOM: u32 = 2;
const MAX_ZOOM: u32 = 13;
const MAX_RENDER_SEGMENT_METERS: f64 = 5_000.0;
const WEB_MERCATOR_MAX_LATITUDE: f64 = 85.051_128_78;
const EARTH_RADIUS_KM: f64 = 6_371.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LongRangeLodTier {
    pub zoom_min: u32,
    pub zoom_max: u32,
    pub epsilon_meters: f64,
    pub max_segment_meters: f64,
    pub curve_guard_meters: f64,
}

impl LongRangeLodTier {
    pub const fn new(
        zoom_min: u32,
        zoom_max: u32,
        epsilon_meters: f64,
        max_segment_meters: f64,
        curve_guard_meters: f64,
    ) -> Self {
        Self {
            zoom_min,
            zoom_max,
            epsilon_meters,
            max_segment_meters,
            curve_guard_meters,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct TileSegment {
    pub zoom: u32,
    pub tile_x: u32,
    pub tile_y: u32,
    pub start: (i32, i32),
    pub end: (i32, i32),
}

type SegmentVisitor<'a> = dyn FnMut(TileSegment) -> Result<(), Error> + 'a;

pub(crate) struct RdpBudget {
    remaining: Option<u64>,
}

impl RdpBudget {
    pub fn new(limit: Option<u64>) -> Self {
        Self { remaining: limit }
    }

    fn reserve_evaluation(&mut self) -> Result<(), Error> {
        if let Some(remaining) = &mut self.remaining {
            if *remaining == 0 {
                return Err(Error::InvalidData(
                    "canonical RDP examined-work limit exceeded".into(),
                ));
            }
            *remaining -= 1;
        }
        Ok(())
    }
}

pub(crate) fn visit_drive_lod_tiers<P, F>(
    points: &[(f32, f32)],
    tiers: &[LongRangeLodTier],
    cancelled: &AtomicBool,
    rdp_budget: &mut RdpBudget,
    pulse: P,
    mut visit: F,
) -> Result<(), Error>
where
    P: Fn(usize) -> Result<(), Error> + Send + Sync,
    F: FnMut(usize, Option<TileSegment>) -> Result<(), Error>,
{
    for (tier_index, tier) in tiers.iter().copied().enumerate() {
        if tier.zoom_min < MIN_ZOOM || tier.zoom_max > MAX_ZOOM || tier.zoom_min > tier.zoom_max {
            return Err(Error::InvalidData(
                "canonical CPU LOD tier is outside z2-z13".into(),
            ));
        }
        let tier_pulse = || pulse(tier_index);
        controlled_checkpoint(cancelled, Some(&tier_pulse))?;
        // Invalid input is a continuity boundary before simplification can erase it.
        let mut run_start = 0;
        for index in 0..=points.len() {
            if index.is_multiple_of(1_024) {
                controlled_checkpoint(cancelled, Some(&tier_pulse))?;
            }
            if index == points.len()
                || !valid_coordinate(f64::from(points[index].0), f64::from(points[index].1))
            {
                if run_start < index {
                    let simplified = simplify_controlled(
                        &points[run_start..index],
                        tier,
                        cancelled,
                        rdp_budget,
                        Some(&tier_pulse),
                    )?;
                    let mut emit = |segment| visit(tier_index, Some(segment));
                    add_drive_multi_zoom(
                        &mut emit,
                        &simplified,
                        tier.zoom_min,
                        tier.zoom_max,
                        cancelled,
                        Some(&tier_pulse),
                    )?;
                }
                run_start = index + 1;
            }
        }
        // Complete the original drive once, regardless of its valid-run count.
        visit(tier_index, None)?;
    }
    Ok(())
}

fn controlled_checkpoint(
    cancelled: &AtomicBool,
    pulse: Option<&(dyn Fn() -> Result<(), Error> + Send + Sync)>,
) -> Result<(), Error> {
    check_cancelled(cancelled)?;
    if let Some(pulse) = pulse {
        pulse()?;
    }
    check_cancelled(cancelled)
}

fn simplify_controlled(
    points: &[(f32, f32)],
    tier: LongRangeLodTier,
    cancelled: &AtomicBool,
    rdp_budget: &mut RdpBudget,
    pulse: Option<&(dyn Fn() -> Result<(), Error> + Send + Sync)>,
) -> Result<Vec<(f32, f32)>, Error> {
    controlled_checkpoint(cancelled, pulse)?;
    if points.len() <= 2 {
        return Ok(points.to_vec());
    }
    let kept = rdp_keep_mask(points, tier.epsilon_meters, cancelled, rdp_budget, pulse)?
        .into_iter()
        .enumerate()
        .filter_map(|(index, keep)| keep.then_some(index))
        .collect::<Vec<_>>();
    if kept.len() <= 1 || tier.max_segment_meters <= 0.0 {
        return Ok(kept.into_iter().map(|index| points[index]).collect());
    }
    let mut result = Vec::with_capacity(kept.len() * 2);
    result.push(points[kept[0]]);
    for (index, window) in kept.windows(2).enumerate() {
        if index.is_multiple_of(64) {
            controlled_checkpoint(cancelled, pulse)?;
        }
        append_guarded(
            points,
            window[0],
            window[1],
            tier.max_segment_meters,
            tier.curve_guard_meters,
            &mut result,
            cancelled,
            pulse,
        )?;
    }
    Ok(result)
}

fn rdp_keep_mask(
    points: &[(f32, f32)],
    epsilon_meters: f64,
    cancelled: &AtomicBool,
    rdp_budget: &mut RdpBudget,
    pulse: Option<&(dyn Fn() -> Result<(), Error> + Send + Sync)>,
) -> Result<Vec<bool>, Error> {
    if points.len() <= 2 {
        return Ok(vec![true; points.len()]);
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    let mut examined = 0usize;
    while let Some((start, end)) = stack.pop() {
        controlled_checkpoint(cancelled, pulse)?;
        if end - start <= 1 {
            continue;
        }
        let mut max_distance = 0.0;
        let mut split = start;
        for (index, point) in points.iter().enumerate().take(end).skip(start + 1) {
            examined = examined.wrapping_add(1);
            if examined.is_multiple_of(1_024) {
                controlled_checkpoint(cancelled, pulse)?;
            }
            rdp_budget.reserve_evaluation()?;
            let distance = perpendicular_distance(*point, points[start], points[end]);
            if distance > max_distance {
                max_distance = distance;
                split = index;
            }
        }
        if max_distance > epsilon_meters {
            keep[split] = true;
            stack.push((start, split));
            stack.push((split, end));
        }
    }
    Ok(keep)
}

#[allow(clippy::too_many_arguments)]
fn append_guarded(
    points: &[(f32, f32)],
    start: usize,
    end: usize,
    max_segment_meters: f64,
    curve_guard_meters: f64,
    output: &mut Vec<(f32, f32)>,
    cancelled: &AtomicBool,
    pulse: Option<&(dyn Fn() -> Result<(), Error> + Send + Sync)>,
) -> Result<(), Error> {
    let mut stack = vec![(start, end)];
    while let Some((range_start, range_end)) = stack.pop() {
        controlled_checkpoint(cancelled, pulse)?;
        if range_start >= range_end {
            continue;
        }
        let mut path_length = 0.0;
        let mut max_perpendicular = 0.0;
        let mut curve_split = None;
        for index in (range_start + 1)..range_end {
            if (index - range_start).is_multiple_of(1_024) {
                controlled_checkpoint(cancelled, pulse)?;
            }
            path_length += distance_meters(points[index - 1], points[index]);
            let perpendicular =
                perpendicular_distance(points[index], points[range_start], points[range_end]);
            if perpendicular > max_perpendicular {
                max_perpendicular = perpendicular;
                curve_split = Some(index);
            }
        }
        path_length += distance_meters(points[range_end - 1], points[range_end]);
        let too_long = path_length > max_segment_meters;
        let too_curvy = curve_guard_meters > 0.0 && max_perpendicular > curve_guard_meters;
        if too_long || too_curvy {
            let midpoint = midpoint_by_path_length(
                points,
                range_start,
                range_end,
                path_length,
                cancelled,
                pulse,
            )?;
            let chosen = if too_curvy {
                curve_split.or(midpoint)
            } else {
                midpoint.or(curve_split)
            };
            if let Some(chosen) = chosen {
                stack.push((chosen, range_end));
                stack.push((range_start, chosen));
                continue;
            }
        }
        if output.last().copied() != Some(points[range_end]) {
            output.push(points[range_end]);
        }
    }
    Ok(())
}

fn midpoint_by_path_length(
    points: &[(f32, f32)],
    start: usize,
    end: usize,
    path_length: f64,
    cancelled: &AtomicBool,
    pulse: Option<&(dyn Fn() -> Result<(), Error> + Send + Sync)>,
) -> Result<Option<usize>, Error> {
    if end <= start + 1 {
        return Ok(None);
    }
    let target = path_length / 2.0;
    let mut walked = 0.0;
    for index in (start + 1)..end {
        if (index - start).is_multiple_of(1_024) {
            controlled_checkpoint(cancelled, pulse)?;
        }
        walked += distance_meters(points[index - 1], points[index]);
        if walked >= target {
            return Ok(Some(index));
        }
    }
    Ok(Some(start + ((end - start) / 2).max(1)))
}

#[inline]
fn distance_meters(a: (f32, f32), b: (f32, f32)) -> f64 {
    let lat1 = f64::from(a.0).to_radians();
    let lat2 = f64::from(b.0).to_radians();
    let delta_lat = (f64::from(b.0) - f64::from(a.0)).to_radians();
    let delta_lon = (f64::from(b.1) - f64::from(a.1)).to_radians();
    let value =
        (delta_lat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (delta_lon / 2.0).sin().powi(2);
    EARTH_RADIUS_KM * 1_000.0 * 2.0 * value.sqrt().asin()
}

#[inline]
fn perpendicular_distance(point: (f32, f32), start: (f32, f32), end: (f32, f32)) -> f64 {
    let lat_m = 111_320.0;
    let lon_m = lat_m * f64::from(start.0).to_radians().cos();
    let px = wrapped_longitude_delta(f64::from(point.1) - f64::from(start.1)) * lon_m;
    let py = f64::from(point.0 - start.0) * lat_m;
    let bx = wrapped_longitude_delta(f64::from(end.1) - f64::from(start.1)) * lon_m;
    let by = f64::from(end.0 - start.0) * lat_m;
    let length_squared = bx * bx + by * by;
    if length_squared < 1e-10 {
        return (px * px + py * py).sqrt();
    }
    let t = ((px * bx + py * by) / length_squared).clamp(0.0, 1.0);
    let dx = px - t * bx;
    let dy = py - t * by;
    (dx * dx + dy * dy).sqrt()
}

#[derive(Clone, Copy)]
struct ProjectedPoint {
    lat: f32,
    lon: f32,
    nx: f64,
    ny: f64,
}

#[allow(clippy::too_many_arguments)]
fn add_drive_multi_zoom(
    output: &mut SegmentVisitor<'_>,
    points: &[(f32, f32)],
    zoom_min: u32,
    zoom_max: u32,
    cancelled: &AtomicBool,
    pulse: Option<&(dyn Fn() -> Result<(), Error> + Send + Sync)>,
) -> Result<(), Error> {
    if points.len() < 2 {
        return Ok(());
    }
    let mut projected = Vec::with_capacity(points.len());
    for (index, &(lat, lon)) in points.iter().enumerate() {
        if index.is_multiple_of(1_024) {
            controlled_checkpoint(cancelled, pulse)?;
        }
        let lat64 = f64::from(lat);
        let lon64 = f64::from(lon);
        projected.push(valid_coordinate(lat64, lon64).then(|| ProjectedPoint {
            lat,
            lon,
            nx: (lon64 + 180.0) / 360.0,
            ny: mercator_y(lat64),
        }));
    }
    for zoom in zoom_min..=zoom_max {
        controlled_checkpoint(cancelled, pulse)?;
        add_projected_drive(output, &projected, zoom, cancelled, pulse)?;
    }
    Ok(())
}

fn add_projected_drive(
    output: &mut SegmentVisitor<'_>,
    points: &[Option<ProjectedPoint>],
    zoom: u32,
    cancelled: &AtomicBool,
    pulse: Option<&(dyn Fn() -> Result<(), Error> + Send + Sync)>,
) -> Result<(), Error> {
    let scale = f64::from(1_u32 << zoom) * f64::from(TILE_SIZE);
    let mut previous = None;
    for (index, point) in points.iter().enumerate() {
        if index.is_multiple_of(1_024) {
            controlled_checkpoint(cancelled, pulse)?;
        }
        let Some(point) = *point else {
            previous = None;
            continue;
        };
        if let Some(start) = previous
            && renderable_segment(start, point)
        {
            add_projected_segment(output, zoom, start, point, scale)?;
        }
        previous = Some(point);
    }
    Ok(())
}

fn add_projected_segment(
    output: &mut SegmentVisitor<'_>,
    zoom: u32,
    start: ProjectedPoint,
    end: ProjectedPoint,
    scale: f64,
) -> Result<(), Error> {
    let delta_x = end.nx - start.nx;
    if delta_x.abs() <= 0.5 {
        return add_world_segment(
            output,
            zoom,
            start.nx * scale,
            start.ny * scale,
            end.nx * scale,
            end.ny * scale,
        );
    }
    let (unwrapped_end_x, boundary_x, first_edge_x, second_edge_x) = if delta_x < 0.0 {
        (end.nx + 1.0, 1.0, scale - 0.0001, 0.0)
    } else {
        (end.nx - 1.0, 0.0, 0.0, scale - 0.0001)
    };
    let denominator = unwrapped_end_x - start.nx;
    if denominator.abs() <= f64::EPSILON {
        // Exact -180/+180 aliases share a meridian. Retain vertical motion
        // on the starting alias's edge, as the same-alias path already does.
        return add_world_segment(
            output,
            zoom,
            start.nx * scale,
            start.ny * scale,
            start.nx * scale,
            end.ny * scale,
        );
    }
    let crossing_t = ((boundary_x - start.nx) / denominator).clamp(0.0, 1.0);
    let edge_y = (start.ny + ((end.ny - start.ny) * crossing_t)) * scale;
    let (start_x, start_y) = (start.nx * scale, start.ny * scale);
    let (end_x, end_y) = (end.nx * scale, end.ny * scale);
    if (start_x - first_edge_x).abs() > f64::EPSILON || (start_y - edge_y).abs() > f64::EPSILON {
        add_world_segment(output, zoom, start_x, start_y, first_edge_x, edge_y)?;
    }
    if (second_edge_x - end_x).abs() > f64::EPSILON || (edge_y - end_y).abs() > f64::EPSILON {
        add_world_segment(output, zoom, second_edge_x, edge_y, end_x, end_y)?;
    }
    Ok(())
}

fn add_world_segment(
    output: &mut SegmentVisitor<'_>,
    zoom: u32,
    start_x: f64,
    start_y: f64,
    end_x: f64,
    end_y: f64,
) -> Result<(), Error> {
    visit_crossed_tiles(start_x, start_y, end_x, end_y, zoom, |tile_x, tile_y| {
        let start = world_to_tile_pixel(start_x, start_y, tile_x, tile_y);
        let end = world_to_tile_pixel(end_x, end_y, tile_x, tile_y);
        if start != end {
            output(TileSegment {
                zoom,
                tile_x,
                tile_y,
                start,
                end,
            })?;
        }
        Ok(())
    })
}

#[inline]
fn mercator_y(latitude: f64) -> f64 {
    let radians = latitude
        .clamp(-WEB_MERCATOR_MAX_LATITUDE, WEB_MERCATOR_MAX_LATITUDE)
        .to_radians();
    ((1.0 - (radians.tan() + 1.0 / radians.cos()).ln() / std::f64::consts::PI) / 2.0)
        .clamp(0.0, 1.0)
}

#[inline]
fn world_to_tile_pixel(world_x: f64, world_y: f64, tile_x: u32, tile_y: u32) -> (i32, i32) {
    let size = f64::from(TILE_SIZE);
    (
        (world_x - f64::from(tile_x) * size).round() as i32,
        (world_y - f64::from(tile_y) * size).round() as i32,
    )
}

fn visit_crossed_tiles(
    start_world_x: f64,
    start_world_y: f64,
    end_world_x: f64,
    end_world_y: f64,
    zoom: u32,
    mut visit: impl FnMut(u32, u32) -> Result<(), Error>,
) -> Result<(), Error> {
    let size = f64::from(TILE_SIZE);
    let max_tile = ((1_u32 << zoom) - 1) as i32;
    let max_world = f64::from(1_u32 << zoom) * size - 0.0001;
    let mut start_x = start_world_x.clamp(0.0, max_world);
    let mut start_y = start_world_y.clamp(0.0, max_world);
    let mut end_x = end_world_x.clamp(0.0, max_world);
    let mut end_y = end_world_y.clamp(0.0, max_world);
    if (start_x % size).abs() < f64::EPSILON && end_x < start_x {
        start_x -= 0.0001;
    }
    if (start_y % size).abs() < f64::EPSILON && end_y < start_y {
        start_y -= 0.0001;
    }
    if (end_x % size).abs() < f64::EPSILON && end_x > start_x {
        end_x -= 0.0001;
    }
    if (end_y % size).abs() < f64::EPSILON && end_y > start_y {
        end_y -= 0.0001;
    }
    let mut tile_x = (start_x / size).floor() as i32;
    let mut tile_y = (start_y / size).floor() as i32;
    let end_tile_x = (end_x / size).floor() as i32;
    let end_tile_y = (end_y / size).floor() as i32;
    let dx = end_x - start_x;
    let dy = end_y - start_y;
    let step_x = dx.partial_cmp(&0.0).map_or(0, |ordering| match ordering {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    });
    let step_y = dy.partial_cmp(&0.0).map_or(0, |ordering| match ordering {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    });
    let inv_dx = if dx.abs() > f64::EPSILON {
        1.0 / dx.abs()
    } else {
        f64::INFINITY
    };
    let inv_dy = if dy.abs() > f64::EPSILON {
        1.0 / dy.abs()
    } else {
        f64::INFINITY
    };
    let mut max_x = if step_x > 0 {
        ((f64::from(tile_x + 1) * size) - start_x) * inv_dx
    } else if step_x < 0 {
        (start_x - f64::from(tile_x) * size) * inv_dx
    } else {
        f64::INFINITY
    };
    let mut max_y = if step_y > 0 {
        ((f64::from(tile_y + 1) * size) - start_y) * inv_dy
    } else if step_y < 0 {
        (start_y - f64::from(tile_y) * size) * inv_dy
    } else {
        f64::INFINITY
    };
    let delta_x = if step_x == 0 {
        f64::INFINITY
    } else {
        size * inv_dx
    };
    let delta_y = if step_y == 0 {
        f64::INFINITY
    } else {
        size * inv_dy
    };
    let mut iterations = 0;
    while tile_x >= 0
        && tile_x <= max_tile
        && tile_y >= 0
        && tile_y <= max_tile
        && iterations < 4096
    {
        visit(tile_x as u32, tile_y as u32)?;
        if tile_x == end_tile_x && tile_y == end_tile_y {
            break;
        }
        if max_x < max_y {
            tile_x += step_x;
            max_x += delta_x;
        } else if max_y < max_x {
            tile_y += step_y;
            max_y += delta_y;
        } else {
            tile_x += step_x;
            tile_y += step_y;
            max_x += delta_x;
            max_y += delta_y;
        }
        iterations += 1;
    }
    Ok(())
}

#[inline]
fn renderable_segment(start: ProjectedPoint, end: ProjectedPoint) -> bool {
    const LAT_M: f64 = 111_320.0;
    let cosine = f64::from(start.lat).to_radians().cos();
    let dy = f64::from(end.lat - start.lat) * LAT_M;
    let dx = wrapped_longitude_delta(f64::from(end.lon) - f64::from(start.lon)) * LAT_M * cosine;
    dy * dy + dx * dx < MAX_RENDER_SEGMENT_METERS * MAX_RENDER_SEGMENT_METERS
}

#[inline]
fn valid_coordinate(lat: f64, lon: f64) -> bool {
    lat.is_finite()
        && lon.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon)
        && !(lat == 0.0 && lon == 0.0)
}

#[inline]
fn wrapped_longitude_delta(delta: f64) -> f64 {
    (delta + 180.0).rem_euclid(360.0) - 180.0
}

#[inline]
fn check_cancelled(cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::SeqCst) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
