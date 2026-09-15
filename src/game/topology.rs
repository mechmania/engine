//! `MapTopology` -- the arena's navigation graph, built once per bot process.
//!
//! MechMania is a strategy contest, so pathfinding is the engine's job, not a competitor's.
//! `init_topology` builds a visibility graph over the arena's wall corners together with
//! all-pairs shortest paths, and `navigate_to` turns it into a velocity. Strategy code never
//! implements a search.
//!
//! # Why corners
//!
//! Shrink the bot to a point and grow every wall by its radius (configuration space). A
//! bot's *centre* may sit at `p` exactly when `p` is outside every inflated block. In such
//! a space a shortest path is a taut string: it bends only where it is pressed against a
//! **convex** corner -- never in open space (a straight line would be shorter) and never at
//! a concave one (that is inside the wall). The convex corners are therefore the complete
//! set of candidate turning points, which is what makes ~120 vertices sufficient on a
//! 32 x 32 grid instead of 1024.
//!
//! Inflating a corner turns it into a quarter-circle arc, and the exact optimum wraps that
//! arc. We put a single vertex on each arc's diagonal instead, offset by `radius + EPSILON`
//! **per axis**. Two errors then cancel -- routing through a point `1.41 * radius` from the
//! corner overshoots, skipping the arc undershoots -- and the residual measures at 0.6% of
//! path length on average, 1.8% worst case, for a sixth of the vertices a real tangent
//! graph would need.
//!
//! The per-axis offset is load-bearing, not a rounding choice. Placing the vertex *on* the
//! arc (offset `(radius + EPSILON) / sqrt(2)` per axis) is tighter but disconnects the
//! graph: the straight chord between two such vertices flanking the same wall face cuts
//! back inside the inflated block, so the two cannot see each other. Measured on the
//! current map, that placement drops the edge count by two thirds and leaves pairs with no
//! route at all.
//!
//! # Mirroring
//!
//! `GameState::mirror` flips the world for team B but `GameConfig` is *not* mirrored -- the
//! map is invariant under `mirror_pos` by construction (see `config::build_map`), and so,
//! therefore, is this graph. Team B queries the same tables with mirrored coordinates and
//! lands on the mirror-image vertex. `topology_test::the_vertex_set_is_mirror_invariant`
//! is what keeps that true.

use std::sync::OnceLock;

use super::config::*;
use super::util::{boxed_zeroed, Vec2, Zeroable};

/// Upper bound on the number of vertices. The whole padded array is allocated whatever the
/// live count, so this is a size choice as much as a limit. `MAP_ART` is still moving, so
/// the live count moves with it -- a hundred or so at the time of writing. The ceiling for
/// any map at any radius is its total convex corner count, which no 32 x 32 arena gets
/// near.
pub const TOPOLOGY_MAX: usize = 256;

/// The arena's navigation graph. Static for the whole match, hence config and not state.
///
/// Laid out with no implicit padding so the hand-written Python (`ctypes`) and Java (JNA)
/// ports can mirror it field for field.
#[derive(Clone, PartialEq, Debug)]
#[repr(C)]
pub struct MapTopology {
    /// How many entries of `verts` are live. A `u32` rather than the `u16` it needs so that
    /// `verts` lands on its natural alignment with nothing implicit in between.
    pub vertex_count: u32,
    /// `verts[..vertex_count]` are the graph's vertices in world space; the rest is padding.
    pub verts: [Vec2; TOPOLOGY_MAX],
    /// `dist[i][j]` is the shortest walkable distance from vertex `i` to vertex `j`, and
    /// `f32::INFINITY` when there is no route -- which is also what every padding row and
    /// column holds.
    pub dist: [[f32; TOPOLOGY_MAX]; TOPOLOGY_MAX],
    /// `next[i][j]` is the vertex to step to from `i` when heading for `j`. Meaningful only
    /// where `dist[i][j]` is finite.
    ///
    /// A `u8` even though `TOPOLOGY_MAX` is 256 and leaves no spare index: unreachability
    /// is carried by `dist`, never by a sentinel here, so every one of `0..=255` is free to
    /// mean a real vertex.
    pub next: [[u8; TOPOLOGY_MAX]; TOPOLOGY_MAX],
}

impl MapTopology {
    /// A graph with no vertices. Every pair is unreachable, so a process that somehow ends
    /// up querying an unbuilt graph fails closed -- `path_length` returns `None` and
    /// `navigate_to` degrades to walking straight at the target.
    pub const EMPTY: Self = Self {
        vertex_count: 0,
        verts: [Vec2::ZERO; TOPOLOGY_MAX],
        dist: [[f32::INFINITY; TOPOLOGY_MAX]; TOPOLOGY_MAX],
        next: [[0; TOPOLOGY_MAX]; TOPOLOGY_MAX],
    };

    pub fn vertices(&self) -> &[Vec2] {
        &self.verts[..self.vertex_count as usize]
    }
}

// ---------------------------------------------------------------------------------------
// geometry
// ---------------------------------------------------------------------------------------

/// Distance from `p` to the segment `a`-`b`.
pub fn point_seg_dist(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    point_seg_dist_sq(p, a, b).sqrt()
}

fn point_seg_dist_sq(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let len_sq = ab.norm_sq();
    if len_sq <= EPSILON * EPSILON {
        return p.dist_sq(&a);
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    p.dist_sq(&(a + ab * t))
}

/// Tiles whose square could come within `radius` of the segment `a`-`b`, as an inclusive
/// `(x0, y0, x1, y1)` index range already clipped to the grid.
fn candidate_tiles(radius: f32, a: Vec2, b: Vec2) -> (usize, usize, usize, usize) {
    let last = (MAP_SIZE - 1) as f32;
    let lo = |v: f32| (v - radius - 1.0).floor().clamp(0.0, last) as usize;
    let hi = |v: f32| (v + radius).floor().clamp(0.0, last) as usize;
    (
        lo(a.x.min(b.x)),
        lo(a.y.min(b.y)),
        hi(a.x.max(b.x)),
        hi(a.y.max(b.y)),
    )
}

/// Whether a bot of `radius` can stand with its centre at `p`: inside the arena and clear
/// of every wall tile.
fn point_free_in(map: &Map, radius: f32, p: Vec2) -> bool {
    let size = MAP_SIZE as f32;
    if p.x < radius || p.y < radius || p.x > size - radius || p.y > size - radius {
        return false;
    }
    let (x0, y0, x1, y1) = candidate_tiles(radius, p, p);
    for x in x0..=x1 {
        for y in y0..=y1 {
            if map[x][y] != MapTile::Wall {
                continue;
            }
            let near = Vec2::new(
                p.x.clamp(x as f32, x as f32 + 1.0),
                p.y.clamp(y as f32, y as f32 + 1.0),
            );
            if near.dist_sq(&p) < radius * radius {
                return false;
            }
        }
    }
    true
}

/// The rays in `corridor_clear_in` only cover the swept barrel while a bot is narrower than
/// a wall block. Checked at compile time for the shipped radius; `MapTopology::build_into`
/// checks whatever radius it is actually handed.
const _BOT_FITS_BETWEEN_THE_RAYS: () = assert!(2.0 * BOT_RADIUS < 1.0);

/// Whether every tile the segment `a`-`b` passes through is clear, walking them with a DDA
/// (Amanatides & Woo).
///
/// `t` runs 0..1 along the segment, `t_max_*` is where it next crosses a grid line, and
/// `t_delta_*` is what one whole tile of travel costs in `t`. An axis with no motion never
/// wins the comparison, which is what the infinities are for.
fn ray_clear_in(map: &Map, a: Vec2, b: Vec2) -> bool {
    let last = MAP_SIZE as isize - 1;
    let cell = |v: f32| (v.floor() as isize).clamp(0, last);
    let (mut x, mut y) = (cell(a.x), cell(a.y));
    let (ex, ey) = (cell(b.x), cell(b.y));

    let d = b - a;
    let axis = |from: f32, delta: f32, at: isize| -> (isize, f32, f32) {
        if delta > 0.0 {
            (1, ((at + 1) as f32 - from) / delta, 1.0 / delta)
        } else if delta < 0.0 {
            (-1, (at as f32 - from) / delta, -1.0 / delta)
        } else {
            (0, f32::INFINITY, f32::INFINITY)
        }
    };
    let (step_x, mut t_max_x, t_delta_x) = axis(a.x, d.x, x);
    let (step_y, mut t_max_y, t_delta_y) = axis(a.y, d.y, y);

    // a straight line across the grid crosses at most `MAP_SIZE` lines per axis, so this
    // bound is only ever reached if float slop has confused the walk
    for _ in 0..(2 * MAP_SIZE + 4) {
        if map[x as usize][y as usize] == MapTile::Wall {
            return false;
        }
        if x == ex && y == ey {
            return true;
        }
        if t_max_x < t_max_y {
            t_max_x += t_delta_x;
            x += step_x;
        } else {
            t_max_y += t_delta_y;
            y += step_y;
        }
        // float slop can step past the target cell; both endpoints are already known to be
        // in bounds, so there is nothing left out there to hit
        if x < 0 || y < 0 || x > last || y > last {
            return true;
        }
    }
    true
}

/// Whether a bot of `radius` can walk the straight line from `a` to `b` -- that is, whether
/// the capsule of that radius around the segment stays inside the arena and misses every
/// wall.
///
/// Two rays offset `radius` either side of the centre line, plus the endpoint disc tests.
/// That is exact here, not an approximation, and both halves are load-bearing:
///
/// - **The rays cover the barrel.** A wall that intersects the swept rectangle without
///   crossing either long edge would have to fit strictly between them, and a convex body
///   between two lines `2 * radius` apart has width under `2 * radius`. Wall blocks are whole
///   tiles and a 1x1 square has minimum width 1, so `2 * radius < 1` rules that out. See
///   `_BOT_FITS_BETWEEN_THE_RAYS`.
/// - **The disc tests cover the end caps**, which the rays do not reach, and the arena
///   bounds: the walkable region is the convex box `[radius, MAP_SIZE - radius]^2`, so
///   testing the two endpoints covers every point between them.
///
/// The exact alternative -- scan the segment's bounding box, taking a segment-to-square
/// distance per tile -- is what this replaced. It is `len^2` tiles for a `len`-tile move on a
/// 45-degree diagonal, at four segment-pair distances each, where the DDA walks the ~`2 * len`
/// tiles the segment actually touches at one array lookup each. Measured 8x on the shipped
/// map's traffic and 10x on long diagonals; `two_rays_agree_with_the_exact_capsule_test`
/// keeps the two honest.
fn corridor_clear_in(map: &Map, radius: f32, a: Vec2, b: Vec2) -> bool {
    if !point_free_in(map, radius, a) || !point_free_in(map, radius, b) {
        return false;
    }
    let d = b - a;
    let len = d.norm();
    if len <= EPSILON {
        return true; // no barrel to sweep -- the disc tests above are the whole answer
    }
    let n = Vec2::new(-d.y / len * radius, d.x / len * radius);
    ray_clear_in(map, a + n, b + n) && ray_clear_in(map, a - n, b - n)
}

/// Whether a disc of `radius` centred at `p` is clear of walls and inside the arena.
///
/// Takes an explicit radius so callers can ask about clearance rather than just fit -- "is
/// there room to stand here with a little margin" is a different question from "does a bot
/// fit here exactly".
pub fn disc_free(conf: &GameConfig, p: Vec2, radius: f32) -> bool {
    point_free_in(&conf.map, radius, p)
}

/// Whether a bot can stand with its centre at `p`.
pub fn point_free(conf: &GameConfig, p: Vec2) -> bool {
    disc_free(conf, p, conf.bot.radius)
}

/// Whether a bot can walk the straight line from `a` to `b` without clipping a wall.
pub fn corridor_clear(conf: &GameConfig, a: Vec2, b: Vec2) -> bool {
    corridor_clear_in(&conf.map, conf.bot.radius, a, b)
}

/// Whether a straight line from `a` to `b` is unobstructed by walls -- a zero-radius
/// sightline, unlike `corridor_clear`, which additionally accounts for a bot's own radius
/// and endpoint clearance.
pub fn has_line_of_sight(conf: &GameConfig, a: Vec2, b: Vec2) -> bool {
    ray_clear_in(&conf.map, a, b)
}

// ---------------------------------------------------------------------------------------
// construction -- engine only
// ---------------------------------------------------------------------------------------

/// The convex corners of the wall set: lattice points with exactly one of the four
/// surrounding tiles solid, paired with the direction `(sx, sy)` that tile lies in.
///
/// Only convex corners are wanted. A taut path never bends at a concave one, so a vertex
/// there would be dead weight in an O(V^3) all-pairs solve.
fn convex_corners(map: &Map) -> Vec<(f32, f32, f32, f32)> {
    let is_wall = |x: isize, y: isize| {
        x >= 0 && y >= 0 && x < MAP_SIZE as isize && y < MAP_SIZE as isize
            && map[x as usize][y as usize] == MapTile::Wall
    };

    let mut out = Vec::new();
    for cx in 0..=MAP_SIZE as isize {
        for cy in 0..=MAP_SIZE as isize {
            // the four tiles meeting at this lattice point, and the direction each lies in
            let quadrants = [
                (is_wall(cx - 1, cy - 1), -1.0, -1.0),
                (is_wall(cx, cy - 1), 1.0, -1.0),
                (is_wall(cx - 1, cy), -1.0, 1.0),
                (is_wall(cx, cy), 1.0, 1.0),
            ];
            let solid: Vec<_> = quadrants.iter().filter(|q| q.0).collect();
            if solid.len() == 1 {
                out.push((cx as f32, cy as f32, solid[0].1, solid[0].2));
            }
        }
    }
    out
}

impl MapTopology {
    /// Builds the graph for a bot of `radius` navigating `map`, into somewhere the caller
    /// already owns. Milliseconds, once, at startup -- the map is a compile-time constant, so
    /// none of this is per-tick work.
    ///
    /// Takes its destination by reference because the value is a third of a megabyte and has
    /// no business on a stack: `GameConfig` is built around one of these in place, and
    /// returning `Self` by value would put it back on the stack at every call site.
    pub fn build_into(dst: &mut Self, map: &Map, radius: f32) {
        // `corridor_clear` is two offset rays, which can only miss a wall thinner than the
        // gap between them. Every route in the graph below is built on that test, so a radius
        // it does not hold for would produce a graph full of edges that clip walls.
        assert!(
            2.0 * radius < 1.0,
            "bot diameter {} is not under one tile; `corridor_clear`'s two rays would let a \
             wall block slip between them",
            2.0 * radius,
        );

        // `dst` may hold an older graph, so every field is reset, not just added to.
        dst.vertex_count = 0;
        dst.verts.fill(Vec2::ZERO);
        for row in dst.dist.iter_mut() {
            row.fill(f32::INFINITY);
        }
        for row in dst.next.iter_mut() {
            row.fill(0);
        }
        let topo = dst;

        // 1. a vertex just outside each convex corner, on its diagonal. The offset is
        //    per-axis rather than along the diagonal; see this module's header for why.
        let offset = radius + EPSILON;
        let mut verts: Vec<Vec2> = convex_corners(map)
            .into_iter()
            .map(|(cx, cy, sx, sy)| Vec2::new(cx - sx * offset, cy - sy * offset))
            // corners buried inside another wall, or pushed off the arena, are not places a
            // bot can stand
            .filter(|v| point_free_in(map, radius, *v))
            .collect();
        // deterministic order: the graph is baked into every match's config, so two builds
        // of the same map must agree bit for bit
        verts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
        verts.dedup_by(|a, b| a.x == b.x && a.y == b.y);

        assert!(
            verts.len() <= TOPOLOGY_MAX,
            "map needs {} navigation vertices but TOPOLOGY_MAX is {TOPOLOGY_MAX}; \
             raise it rather than truncating, which would silently corrupt every route",
            verts.len(),
        );

        let n = verts.len();
        topo.vertex_count = n as u32;
        topo.verts[..n].copy_from_slice(&verts);

        // 2. an edge wherever a bot can walk the straight line between two vertices. The
        //    weight is just the distance -- that is what "the straight line is walkable"
        //    means.
        for i in 0..n {
            topo.dist[i][i] = 0.0;
            topo.next[i][i] = i as u8;
            for j in (i + 1)..n {
                if corridor_clear_in(map, radius, verts[i], verts[j]) {
                    let w = verts[i].dist(&verts[j]);
                    topo.dist[i][j] = w;
                    topo.dist[j][i] = w;
                    topo.next[i][j] = j as u8;
                    topo.next[j][i] = i as u8;
                }
            }
        }

        // 3. Floyd-Warshall. O(V^3), but V is ~120 and this runs once.
        for k in 0..n {
            for i in 0..n {
                let d_ik = topo.dist[i][k];
                if !d_ik.is_finite() {
                    continue;
                }
                for j in 0..n {
                    let candidate = d_ik + topo.dist[k][j];
                    if candidate < topo.dist[i][j] {
                        topo.dist[i][j] = candidate;
                        topo.next[i][j] = topo.next[i][k];
                    }
                }
            }
        }
    }

    /// `build_into` a fresh heap allocation.
    pub fn build(map: &Map, radius: f32) -> Box<Self> {
        // Zeroed rather than `Box::new(Self::EMPTY)`, which would materialize a third of a
        // megabyte on the stack before moving it to the heap. `build_into` overwrites every
        // field anyway.
        let mut topo: Box<Self> = boxed_zeroed();
        Self::build_into(&mut topo, map, radius);
        topo
    }
}

// SAFETY: plain `#[repr(C)]` data -- a `u32` and three arrays of `f32`/`Vec2`/`u8`, all of
// which are valid at any bit pattern. `build_into` overwrites all of it regardless.
unsafe impl Zeroable for MapTopology {}

/// This process's navigation graph.
///
/// A singleton rather than a field of `GameConfig`: the graph is a third of a megabyte and a
/// pure function of the map and the bot radius, both fixed for a process's lifetime, so
/// there is no reason to carry it through the handshake -- and every reason not to, since
/// `GameConfig` is embedded in a `Frame` that lives in shared memory.
///
/// The engine never touches this. It is the bots that navigate.
static TOPOLOGY: OnceLock<(u64, Box<MapTopology>)> = OnceLock::new();

/// Cheap fingerprint of the inputs a graph was built from, so a second `init_topology` with
/// *different* inputs can be caught rather than silently ignored.
fn topology_fingerprint(map: &Map, radius: f32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a
    let mut mix = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    };
    for column in map {
        for tile in column {
            mix(*tile as u8);
        }
    }
    for b in radius.to_le_bytes() {
        mix(b);
    }
    h
}

/// Builds the graph for `map` and a bot of `radius` and installs it for this process.
///
/// Idempotent -- a second call is ignored, graph and all. Bots call this once, at handshake,
/// with the map and radius the engine just sent them; it costs milliseconds and the
/// handshake is not charged against the compute budget.
pub fn init_topology(map: &Map, radius: f32) {
    let fingerprint = topology_fingerprint(map, radius);
    match TOPOLOGY.get() {
        // A bot handshakes once, so this only comes up in tests -- where silently keeping
        // the first arena and navigating a second one against it would be a bad afternoon.
        Some((installed, _)) => debug_assert_eq!(
            *installed, fingerprint,
            "init_topology called again with a different map or radius; the first graph is \
             what every later query will use",
        ),
        None => {
            let _ = TOPOLOGY.set((fingerprint, MapTopology::build(map, radius)));
        }
    }
}

/// This process's navigation graph.
///
/// Panics if `init_topology` never ran. Deliberately not a fail-closed `EMPTY`: that would
/// have to be a `static`, and a third of a megabyte of `f32::INFINITY` is not worth carrying
/// in every bot binary to soften a bug that can only be "the handshake did not happen".
pub fn topology() -> &'static MapTopology {
    &TOPOLOGY
        .get()
        .expect("navigation graph not built -- `init_topology` runs at handshake")
        .1
}

// ---------------------------------------------------------------------------------------
// queries -- shared with bots
// ---------------------------------------------------------------------------------------

/// The cheapest route from `from` to `to` that goes through the graph, as
/// `(first vertex, last vertex, total length)`.
///
/// A* over `{from} + vertices + {to}`, with `h(v) = |v - to|` -- admissible because no
/// walkable path is shorter than the straight line, and consistent because every graph edge
/// is at least as long as the straight line too. `dist` is already all-pairs, so relaxing
/// from a single settled vertex hands every other vertex its final cost: the question is
/// really only "which corner do we enter at, and which do we leave from", asked in cost
/// order.
///
/// Asking it in cost order is the entire point, because it makes both visibility fans lazy.
/// Answering it exhaustively -- `min over s, t of |from - s| + dist[s][t] + |t - to|` -- is a
/// couple of hundred table lookups, which is nothing, wrapped around `2 * V` `corridor_clear`
/// calls, which is not: one fan from each end, paid in full every time. A* pays for a vertex
/// only when it reaches the front of the queue and stops at the first vertex that can see
/// `to`. On the shipped map that is 58 calls against 220, and the worst pair sampled still
/// came in under the exhaustive count, so there is no bad tail to trade for the average.
///
/// Selection is a linear scan rather than a binary heap. `V` is ~110 and a scan of that is
/// tens of nanoseconds, but more to the point it keeps the whole search in these five stack
/// arrays -- this runs inside a bot's per-tick budget, where not allocating is worth more
/// than the asymptotics.
fn best_route(conf: &GameConfig, from: Vec2, to: Vec2) -> Option<(u8, u8, f32)> {
    let topo = topology();
    let n = topo.vertex_count as usize;

    // `g[i]`: the cost of the best route to `i` that has been *established*, entry edge
    // vouched for. `direct[i]`: the cost of entering the graph at `i` straight from `from`,
    // which is only a candidate until `corridor_clear` has agreed -- deferring that call is
    // the whole trick, so it is held apart from `g` rather than folded into it, and set to
    // infinity once spent either way. `entry[i]`: which vertex the best route to `i` entered
    // the graph at, which is half of what the caller asked for.
    let mut h = [f32::INFINITY; TOPOLOGY_MAX];
    let mut g = [f32::INFINITY; TOPOLOGY_MAX];
    let mut direct = [f32::INFINITY; TOPOLOGY_MAX];
    let mut entry = [0u8; TOPOLOGY_MAX];
    let mut settled = [false; TOPOLOGY_MAX];
    for i in 0..n {
        h[i] = topo.verts[i].dist(&to);
        direct[i] = from.dist(&topo.verts[i]);
    }

    // Each pass either settles a vertex or spends one candidate entry edge, and there are
    // `n` of each, so this terminates without needing a guard.
    loop {
        let mut at = usize::MAX;
        let mut best_f = f32::INFINITY;
        for i in 0..n {
            if settled[i] {
                continue;
            }
            let f = g[i].min(direct[i]) + h[i];
            if f < best_f {
                best_f = f;
                at = i;
            }
        }
        if at == usize::MAX {
            return None; // nothing open is reachable -- one end is somewhere no bot can stand
        }

        if direct[at] < g[at] {
            // the cheapest thing left is an unvouched-for entry edge, so its turn to be paid
            // for has come -- and it is never paid for twice, cleared or not
            let cost = direct[at];
            direct[at] = f32::INFINITY;
            if !corridor_clear(conf, from, topo.verts[at]) {
                continue; // the graph may still reach it the long way round; leave it open
            }
            g[at] = cost;
            entry[at] = at as u8;
        }
        settled[at] = true;

        // `best_f` is a lower bound on every route still open, and a clear shot from here to
        // `to` costs exactly `best_f` -- so this is the answer, with no fan left to finish.
        if corridor_clear(conf, topo.verts[at], to) {
            return Some((entry[at], at as u8, g[at] + h[at]));
        }

        // one relaxation from a settled vertex is final, `dist` being all-pairs. An
        // unreachable pair is `INFINITY` there, and no comparison against it succeeds.
        for j in 0..n {
            if settled[j] {
                continue;
            }
            let candidate = g[at] + topo.dist[at][j];
            if candidate < g[j] {
                g[j] = candidate;
                entry[j] = entry[at];
            }
        }
    }
}

/// Walking distance from `from` to `to`, going around walls. `None` when no route exists --
/// normally because one end is somewhere a bot cannot stand.
///
/// Use this to *choose* between destinations; use `navigate_to` to actually go.
pub fn path_length(conf: &GameConfig, from: Vec2, to: Vec2) -> Option<f32> {
    if corridor_clear(conf, from, to) {
        return Some(from.dist(&to));
    }
    best_route(conf, from, to).map(|(_, _, len)| len)
}

/// The interior waypoints of the route from `from` to `to`, in order and excluding both
/// ends. Empty when the two see each other directly, `None` when there is no route.
///
/// This is the whole plan, so it allocates -- use it to *inspect* a route (measure it, draw
/// it, pick a spot along it), and `navigate_to` to actually walk one.
pub fn route_waypoints(conf: &GameConfig, from: Vec2, to: Vec2) -> Option<Vec<Vec2>> {
    if corridor_clear(conf, from, to) {
        return Some(Vec::new());
    }
    let (first, last, _) = best_route(conf, from, to)?;
    let topo = topology();

    let mut out = vec![topo.verts[first as usize]];
    let mut at = first;
    while at != last {
        if out.len() > topo.vertex_count as usize {
            return None; // a cycle means the matrix is corrupt; do not spin on it
        }
        at = topo.next[at as usize][last as usize];
        out.push(topo.verts[at as usize]);
    }
    Some(out)
}

/// The direction to move this tick to get from `from` towards `to` around walls. Feed it
/// straight to `move_bot`; the result is a delta, not a unit vector, and `MoveAction`
/// normalizes.
///
/// Call it every tick with the bot's current position -- it is a step, not a plan, and it
/// re-routes for free as the bot moves.
pub fn navigate_to(conf: &GameConfig, from: Vec2, to: Vec2) -> Vec2 {
    if corridor_clear(conf, from, to) {
        return to - from;
    }

    let Some((first, last, _)) = best_route(conf, from, to) else {
        // Nowhere to route through -- wedged against a wall, or the target is somewhere no
        // bot can stand. Shove at it and let `handle_collision` slide us along the face.
        return to - from;
    };

    // String-pulling: the route's first hop is a valid aim, but a later one may already be
    // in sight, and cutting to it saves the dogleg. Walk forward while we can still see the
    // next vertex, and stop at the first we cannot -- that one is a corner we have to
    // round.
    let topo = topology();
    let mut aim = first;
    let mut steps = 0;
    while aim != last && steps <= topo.vertex_count {
        let hop = topo.next[aim as usize][last as usize];
        if !corridor_clear(conf, from, topo.verts[hop as usize]) {
            break;
        }
        aim = hop;
        steps += 1;
    }

    topo.verts[aim as usize] - from
}

// Plain `cfg(test)`: nothing here is engine-only, and the FFI conformance test in
// `ffi.rs` reuses this module's exact-geometry oracles from a `client` build, where
// `feature = "engine"` is off by construction (see the guard in `lib.rs`).
#[cfg(test)]
mod topology_test {
    use super::*;

    use crate::game::config::test_conf::{conf, sample_free_points, Rng};

    fn topo() -> &'static MapTopology {
        conf();
        topology()
    }


    #[test]
    fn every_vertex_is_standable() {
        let conf = conf();
        for (i, v) in topo().vertices().iter().enumerate() {
            assert!(point_free(conf, *v), "vertex {i} at {v:?} is inside a wall");
        }
    }

    #[test]
    fn every_edge_is_walkable() {
        let conf = conf();
        let topo = topo();
        let n = topo.vertex_count as usize;
        for i in 0..n {
            for j in 0..n {
                if i == j {
                    continue;
                }
                // a one-hop route is an edge, and an edge must be a walkable straight line
                if topo.next[i][j] as usize == j && topo.dist[i][j].is_finite() {
                    let (a, b) = (topo.verts[i], topo.verts[j]);
                    assert!(corridor_clear(conf, a, b), "edge {i}-{j} clips a wall");
                    assert!((topo.dist[i][j] - a.dist(&b)).abs() < 1e-3);
                }
            }
        }
    }

    #[test]
    fn the_graph_is_connected() {
        let topo = topo();
        let n = topo.vertex_count as usize;
        assert!(n > 0);
        for i in 0..n {
            for j in 0..n {
                assert!(
                    topo.dist[i][j].is_finite(),
                    "no route from vertex {i} to vertex {j} -- the arena has a sealed pocket",
                );
            }
        }
    }

    /// The load-bearing one. `GameConfig` is not mirrored for team B -- only `GameState` is
    /// -- so B queries these very tables with mirrored coordinates. That is only sound
    /// because the vertex set maps onto itself under `mirror_pos`.
    #[test]
    fn the_vertex_set_is_mirror_invariant() {
        let topo = topo();
        for v in topo.vertices() {
            let mut m = *v;
            mirror_pos(&mut m);
            assert!(
                topo.vertices().iter().any(|w| w.dist(&m) < 1e-3),
                "{v:?} mirrors to {m:?}, which is not a vertex",
            );
        }
    }

    #[test]
    fn distances_are_symmetric_under_mirroring() {
        let topo = topo();
        let n = topo.vertex_count as usize;
        let image = |i: usize| {
            let mut m = topo.verts[i];
            mirror_pos(&mut m);
            (0..n).find(|&k| topo.verts[k].dist(&m) < 1e-3).unwrap()
        };
        for i in 0..n {
            for j in 0..n {
                let (mi, mj) = (image(i), image(j));
                let (a, b) = (topo.dist[i][j], topo.dist[mi][mj]);
                assert!((a - b).abs() < 1e-2, "dist {i}->{j} is {a} but its mirror is {b}");
            }
        }
    }

    #[test]
    fn distances_are_symmetric_and_obey_the_triangle_inequality() {
        let topo = topo();
        let n = topo.vertex_count as usize;
        for i in 0..n {
            assert_eq!(topo.dist[i][i], 0.0);
            for j in 0..n {
                assert!((topo.dist[i][j] - topo.dist[j][i]).abs() < 1e-3);
                for k in (0..n).step_by(7) {
                    assert!(
                        topo.dist[i][j] <= topo.dist[i][k] + topo.dist[k][j] + 1e-2,
                        "triangle inequality violated for {i}, {k}, {j}",
                    );
                }
            }
        }
    }

    #[test]
    fn padding_is_unreachable() {
        let topo = topo();
        let n = topo.vertex_count as usize;
        assert!(n < TOPOLOGY_MAX, "no padding left to check");
        for i in n..TOPOLOGY_MAX {
            for j in 0..TOPOLOGY_MAX {
                assert!(topo.dist[i][j].is_infinite());
                assert!(topo.dist[j][i].is_infinite());
            }
        }
    }

    /// Floyd-Warshall against an independent Dijkstra over the same edges.
    #[test]
    fn matches_dijkstra() {
        let conf = conf();
        let topo = topo();
        let n = topo.vertex_count as usize;
        let adjacency: Vec<Vec<(usize, f32)>> = (0..n)
            .map(|i| {
                (0..n)
                    .filter(|&j| i != j && corridor_clear(conf, topo.verts[i], topo.verts[j]))
                    .map(|j| (j, topo.verts[i].dist(&topo.verts[j])))
                    .collect()
            })
            .collect();

        for source in (0..n).step_by(11) {
            let mut best = vec![f32::INFINITY; n];
            let mut done = vec![false; n];
            best[source] = 0.0;
            loop {
                let Some(u) = (0..n)
                    .filter(|&i| !done[i] && best[i].is_finite())
                    .min_by(|&a, &b| best[a].total_cmp(&best[b]))
                else {
                    break;
                };
                done[u] = true;
                for &(v, w) in &adjacency[u] {
                    if best[u] + w < best[v] {
                        best[v] = best[u] + w;
                    }
                }
            }
            for j in 0..n {
                assert!(
                    (topo.dist[source][j] - best[j]).abs() < 1e-2,
                    "dist[{source}][{j}] is {} but Dijkstra says {}",
                    topo.dist[source][j],
                    best[j],
                );
            }
        }
    }

    #[test]
    fn build_is_deterministic() {
        let a = MapTopology::build(&MAP, BOT_RADIUS);
        let b = MapTopology::build(&MAP, BOT_RADIUS);
        assert!(*a == *b);
    }

    /// Everything the match actually needs to reach has to be reachable: both spawns, both
    /// goals, and every waypoint the payload passes through. This is what the deleted
    /// the arena-connectivity test in `config` used to be for, against the real bot radius
    /// rather than a 3x3 tile rule.
    #[test]
    fn the_match_critical_points_are_all_connected() {
        let conf = conf();
        let mut required = vec![payload_position(-1.0), payload_position(0.0), payload_position(1.0)];
        required.extend(PAYLOAD_PATH);

        for &a in &required {
            for &b in &required {
                assert!(
                    path_length(conf, a, b).is_some(),
                    "no route from {a:?} to {b:?}",
                );
            }
        }
    }

    /// The route `navigate_to` follows must be walkable leg by leg, and its legs must add up
    /// to the length `path_length` advertises.
    #[test]
    fn routes_are_walkable_and_add_up() {
        let conf = conf();
        let topo = topo();
        let points = sample_free_points(40);
        for pair in points.chunks(2).filter(|c| c.len() == 2) {
            let (from, to) = (pair[0], pair[1]);
            let Some((first, last, len)) = best_route(conf, from, to) else {
                continue;
            };

            let mut walked = from.dist(&topo.verts[first as usize]);
            let mut at = first;
            let mut steps = 0;
            while at != last {
                let hop = topo.next[at as usize][last as usize];
                let (a, b) = (topo.verts[at as usize], topo.verts[hop as usize]);
                assert!(corridor_clear(conf, a, b), "leg {at}-{hop} clips a wall");
                walked += a.dist(&b);
                at = hop;
                steps += 1;
                assert!(steps <= topo.vertex_count, "route cycles");
            }
            walked += topo.verts[last as usize].dist(&to);

            assert!(
                (walked - len).abs() < 1e-2,
                "route from {from:?} to {to:?} advertises {len} but its legs sum to {walked}",
            );
        }
    }

    /// End to end: a bot steered only by `navigate_to` gets where it is going without ever
    /// touching a wall.
    #[test]
    fn navigate_to_arrives_without_clipping_a_wall() {
        let conf = conf();
        let speed = conf.bot.speed.value[0];
        let points = sample_free_points(24);

        for pair in points.chunks(2).filter(|c| c.len() == 2) {
            let (start, target) = (pair[0], pair[1]);
            let Some(expected) = path_length(conf, start, target) else {
                continue;
            };

            let mut pos = start;
            // generous: the polyline is within a couple of percent of optimal, and the
            // budget only has to rule out wandering
            let budget = (expected / speed * 1.5) as usize + 64;
            let mut arrived = false;
            for _ in 0..budget {
                if pos.dist(&target) <= speed {
                    arrived = true;
                    break;
                }
                let step = navigate_to(conf, pos, target).normalize_or_zero() * speed;
                pos = pos + step;
                assert!(
                    point_free(conf, pos),
                    "navigating {start:?} -> {target:?} put a bot inside a wall at {pos:?}",
                );
            }
            assert!(
                arrived,
                "navigating {start:?} -> {target:?} (route {expected}) did not arrive in {budget} ticks, \
                 stalled at {pos:?}",
            );
        }
    }

    #[test]
    #[ignore = "timing, not a correctness check -- cargo test --release -- --ignored bench"]
    fn bench_navigate_to() {
        let conf = conf();
        let points = sample_free_points(200);
        let start = std::time::Instant::now();
        let mut sink = 0.0f32;
        for pair in points.chunks(2) {
            for _ in 0..50 {
                sink += navigate_to(conf, pair[0], pair[1]).x;
            }
        }
        let each = start.elapsed() / 5000;
        println!("\nnavigate_to: {each:?} per call ({sink})");
        println!("a 32-bot fleet, every tick: {:?}", each * 32);
    }


    // -----------------------------------------------------------------------------------
    // Oracles. Both of these are the implementation that used to ship, kept because the
    // things that replaced them trade exhaustiveness for speed, and the cheapest way to keep
    // that honest is to still have the exhaustive one to ask.
    // -----------------------------------------------------------------------------------

    /// Squared distance between two segments. Zero when they cross.
    fn seg_seg_dist_sq(p1: Vec2, p2: Vec2, q1: Vec2, q2: Vec2) -> f32 {
        let d1 = p2 - p1;
        let d2 = q2 - q1;
        let den = d1.x * d2.y - d1.y * d2.x;
        if den.abs() > 1e-12 {
            let r = q1 - p1;
            let t = (r.x * d2.y - r.y * d2.x) / den;
            let u = (r.x * d1.y - r.y * d1.x) / den;
            if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
                return 0.0;
            }
        }
        point_seg_dist_sq(p1, q1, q2)
            .min(point_seg_dist_sq(p2, q1, q2))
            .min(point_seg_dist_sq(q1, p1, p2))
            .min(point_seg_dist_sq(q2, p1, p2))
    }

    /// Squared distance from the segment `a`-`b` to tile `(x, y)`'s unit square. Exact, and
    /// zero when the segment enters the square.
    fn seg_tile_dist_sq(a: Vec2, b: Vec2, x: usize, y: usize) -> f32 {
        let (lo, hi) = (
            Vec2::new(x as f32, y as f32),
            Vec2::new(x as f32 + 1.0, y as f32 + 1.0),
        );
        let inside = |p: Vec2| p.x >= lo.x && p.x <= hi.x && p.y >= lo.y && p.y <= hi.y;
        if inside(a) || inside(b) {
            return 0.0;
        }
        let corners = [lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)];
        let mut best = f32::INFINITY;
        for i in 0..4 {
            best = best.min(seg_seg_dist_sq(a, b, corners[i], corners[(i + 1) % 4]));
        }
        best
    }

    /// `corridor_clear` the slow, unarguable way: the exact capsule-to-square distance for
    /// every tile in the segment's bounding box.
    fn corridor_clear_exact(conf: &GameConfig, a: Vec2, b: Vec2) -> bool {
        let (map, radius) = (&conf.map, conf.bot.radius);
        if !point_free_in(map, radius, a) || !point_free_in(map, radius, b) {
            return false;
        }
        let (x0, y0, x1, y1) = candidate_tiles(radius, a, b);
        for x in x0..=x1 {
            for y in y0..=y1 {
                if map[x][y] == MapTile::Wall
                    && seg_tile_dist_sq(a, b, x, y) < radius * radius
                {
                    return false;
                }
            }
        }
        true
    }

    /// `best_route` the exhaustive way: both visibility fans in full, then every
    /// entry/exit pair.
    fn best_route_exhaustive(conf: &GameConfig, from: Vec2, to: Vec2) -> Option<(u8, u8, f32)> {
        let topo = topology();
        let exits: Vec<(usize, f32)> = topo
            .vertices()
            .iter()
            .enumerate()
            .filter(|(_, v)| corridor_clear(conf, to, **v))
            .map(|(j, v)| (j, to.dist(v)))
            .collect();

        let mut best: Option<(u8, u8, f32)> = None;
        for (i, v) in topo.vertices().iter().enumerate() {
            if !corridor_clear(conf, from, *v) {
                continue;
            }
            let entry = from.dist(v);
            for &(j, exit) in &exits {
                let across = topo.dist[i][j];
                if !across.is_finite() {
                    continue;
                }
                let total = entry + across + exit;
                if best.map_or(true, |(_, _, b)| total < b) {
                    best = Some((i as u8, j as u8, total));
                }
            }
        }
        best
    }

    /// Segments across the arena, plus the long 45-degree runs that are the two-ray test's
    /// least favourite case and the bounding-box scan's worst one.
    fn corridor_samples() -> Vec<(Vec2, Vec2)> {
        let points = sample_free_points(600);
        let mut out: Vec<(Vec2, Vec2)> = points
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| (c[0], c[1]))
            .collect();
        let mut rng = Rng(0xd1a6_0f1a_1111_2222);
        for _ in 0..400 {
            let a = Vec2::new(rng.next_f32() * 8.0 + 1.0, rng.next_f32() * 8.0 + 1.0);
            let len = 4.0 + rng.next_f32() * 22.0;
            out.push((a, a + Vec2::new(len, len)));
            out.push((a, a + Vec2::new(len, -len)));
            out.push((a, a + Vec2::new(len, 0.0)));
            out.push((a, a + Vec2::new(0.0, len)));
        }
        out
    }

    /// The two rays have to agree with the exact capsule test, or every route in the graph is
    /// suspect. This is what licenses the swap.
    #[test]
    fn two_rays_agree_with_the_exact_capsule_test() {
        let conf = conf();
        let samples = corridor_samples();
        let mut disagree = Vec::new();
        for &(a, b) in &samples {
            if corridor_clear(conf, a, b) != corridor_clear_exact(conf, a, b) {
                disagree.push((a, b));
            }
        }
        assert!(
            disagree.is_empty(),
            "{} of {} segments disagree, first: {:?}",
            disagree.len(),
            samples.len(),
            disagree.first(),
        );
    }

    /// A* must find the same route length the exhaustive search does. It is allowed to pick a
    /// different pair of corners when two routes tie -- only the length is the contract.
    #[test]
    fn astar_agrees_with_the_exhaustive_route_search() {
        let conf = conf();
        let points = sample_free_points(200);
        let mut compared = 0;
        for pair in points.chunks(2).filter(|c| c.len() == 2) {
            let (from, to) = (pair[0], pair[1]);
            match (best_route(conf, from, to), best_route_exhaustive(conf, from, to)) {
                (None, None) => {}
                (Some((_, _, fast)), Some((_, _, slow))) => {
                    assert!(
                        (fast - slow).abs() < 1e-3,
                        "{from:?} -> {to:?}: A* {fast}, exhaustive {slow}",
                    );
                    compared += 1;
                }
                (a, b) => panic!("{from:?} -> {to:?}: A* {a:?}, exhaustive {b:?}"),
            }
        }
        assert!(
            compared > 50,
            "only {compared} routable pairs -- the sample is not exercising this",
        );
    }

    /// Not an assertion, a picture. `cargo test -- --nocapture render_graph`
    #[test]
    fn render_graph() {
        let topo = topo();
        let mut marked = [[false; MAP_SIZE]; MAP_SIZE];
        for v in topo.vertices() {
            marked[(v.x as usize).min(MAP_SIZE - 1)][(v.y as usize).min(MAP_SIZE - 1)] = true;
        }
        println!("\n{} vertices", topo.vertex_count);
        for y in (0..MAP_SIZE).rev() {
            let row: String = (0..MAP_SIZE)
                .map(|x| match (MAP[x][y], marked[x][y]) {
                    (MapTile::Wall, _) => '#',
                    (_, true) => '*',
                    _ => '.',
                })
                .collect();
            println!("{y:>2} {row}");
        }
    }
}
