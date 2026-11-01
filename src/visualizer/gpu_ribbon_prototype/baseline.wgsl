// PROTOTYPE: one static vertex/index upload, one new projected ring slot/frame.
// The render pass reconstructs two time intervals without CPU ribbon geometry.
struct FrameParams {
    rotation: array<vec4<f32>, 3>,
    viewport: vec4<f32>,
    camera: vec4<f32>,
    pose: vec4<f32>,
    style: vec4<f32>,
    tint: vec4<f32>,
    history: vec4<u32>,
}

@group(0) @binding(0) var<uniform> project_params: FrameParams;
@group(0) @binding(1) var<storage, read> vertices: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> projection_ring: array<vec4<f32>>;

@compute @workgroup_size(64)
fn project_vertices(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= project_params.history.x { return; }
    let p = vertices[id.x].xyz;
    let rotated = project_params.rotation[0].xyz * p.x
        + project_params.rotation[1].xyz * p.y
        + project_params.rotation[2].xyz * p.z;
    let raw_depth = rotated.z + project_params.pose.x;
    let depth = max(raw_depth, project_params.camera.w);
    let perspective = project_params.camera.z / depth;
    let screen = project_params.camera.xy
        + vec2(rotated.x, -rotated.y) * perspective + project_params.pose.yz;
    let radius = max(project_params.pose.w, 1e-6);
    let fraction = clamp((depth - (project_params.pose.x - radius)) / (radius * 2.0), 0.0, 1.0);
    projection_ring[project_params.history.y * project_params.history.x + id.x]
        = vec4(screen, 1.0 - fraction * 0.5, select(0.0, 1.0, raw_depth > project_params.camera.w));
}

@group(1) @binding(0) var<uniform> params: FrameParams;
@group(1) @binding(1) var<storage, read> edges: array<vec2<u32>>;
@group(1) @binding(2) var<storage, read> history: array<vec4<f32>>;

struct RibbonVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) first: vec4<f32>,
    @location(1) @interpolate(flat) last: vec4<f32>,
    @location(2) @interpolate(flat) depths: vec4<f32>,
    @location(3) @interpolate(flat) interval: u32,
    @location(4) @interpolate(flat) fast_mode: u32,
    @location(5) ink: vec4<f32>,
    @location(6) @interpolate(flat) support_a: vec4<f32>,
    @location(7) @interpolate(flat) support_b: vec4<f32>,
}

fn cross2(a: vec2<f32>, b: vec2<f32>) -> f32 { return a.x * b.y - a.y * b.x; }

fn interval_vertex(corner: u32, instance: u32) -> RibbonVertex {
    let edge = edges[instance / 2u];
    let interval = instance % 2u;
    let first_slot = (params.history.z + interval) % 3u;
    let last_slot = (first_slot + 1u) % 3u;
    let a0 = history[first_slot * params.history.x + edge.x];
    let b0 = history[first_slot * params.history.x + edge.y];
    let a1 = history[last_slot * params.history.x + edge.x];
    let b1 = history[last_slot * params.history.x + edge.y];
    var result: RibbonVertex;
    result.first = vec4(a0.xy, b0.xy);
    result.last = vec4(a1.xy, b1.xy);
    result.depths = vec4(a0.z, b0.z, a1.z, b1.z);
    result.interval = interval;
    result.fast_mode = 1u;
    result.ink = vec4(0.0);
    result.support_a = vec4(0.0);
    result.support_b = vec4(0.0);
    if min(min(a0.w, b0.w), min(a1.w, b1.w)) < 0.5 {
        result.position = vec4(-2.0, -2.0, 0.0, 1.0);
        return result;
    }
    let e0 = b0.xy - a0.xy;
    let e1 = b1.xy - a1.xy;
    // Align the support to its dominant spatial extent. Tiny edges moving far
    // along a diagonal must align to motion, otherwise an edge-aligned box
    // rasterizes an enormous mostly-empty rectangle around the real sweep.
    var along = e0 + e1;
    let da = (a1.xy - a0.xy) * 2.0;
    let db = (b1.xy - b0.xy) * 2.0;
    if dot(da, da) > dot(along, along) { along = da; }
    if dot(db, db) > dot(along, along) { along = db; }
    if dot(along, along) < 1e-8 { along = e0; }
    if dot(along, along) < 1e-8 { along = vec2(1.0, 0.0); }
    along = normalize(along);
    let across = vec2(-along.y, along.x);
    let points = array<vec2<f32>, 4>(a0.xy, b0.xy, a1.xy, b1.xy);
    var low = vec2(1e20);
    var high = vec2(-1e20);
    for (var i = 0u; i < 4u; i += 1u) {
        let local = vec2(dot(points[i], along), dot(points[i], across));
        low = min(low, local);
        high = max(high, local);
    }
    let padding = max(params.style.x, 1.0) * 0.5 + 0.75;
    low -= vec2(padding);
    high += vec2(padding);
    let corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
    );
    let local = mix(low, high, corners[corner]);
    let point = along * local.x + across * local.y;
    result.position = vec4(point.x / params.viewport.x * 2.0 - 1.0,
        1.0 - point.y / params.viewport.y * 2.0, 0.0, 1.0);
    return result;
}

@vertex
fn ribbon_vertex(
    @builtin(vertex_index) corner: u32,
    @builtin(instance_index) instance: u32,
) -> RibbonVertex {
    return interval_vertex(corner, instance);
}

fn support_edge(a: vec2<f32>, b: vec2<f32>, enabled: bool) -> vec4<f32> {
    let edge = b - a;
    let inverse_length = inverseSqrt(max(dot(edge, edge), 1e-12));
    let normal = vec2(-edge.y, edge.x) * inverse_length;
    return vec4(normal, -dot(normal, a), select(0.0, 1.0, enabled));
}

fn folded_quad(a: vec2<f32>, b: vec2<f32>, c: vec2<f32>, d: vec2<f32>) -> bool {
    let turns = vec4(cross2(b - a, c - b), cross2(c - b, d - c),
        cross2(d - c, a - d), cross2(a - d, b - a));
    return min(min(turns.x, turns.y), min(turns.z, turns.w)) < -1e-4
        && max(max(turns.x, turns.y), max(turns.z, turns.w)) > 1e-4;
}

@vertex
fn ribbon_fast_vertex(
    @builtin(vertex_index) corner: u32,
    @builtin(instance_index) edge_index: u32,
) -> RibbonVertex {
    let edge = edges[edge_index];
    let oldest = params.history.z;
    let middle = (oldest + 1u) % 3u;
    let newest = (oldest + 2u) % 3u;
    let a0 = history[oldest * params.history.x + edge.x];
    let b0 = history[oldest * params.history.x + edge.y];
    let am = history[middle * params.history.x + edge.x];
    let bm = history[middle * params.history.x + edge.y];
    let a1 = history[newest * params.history.x + edge.x];
    let b1 = history[newest * params.history.x + edge.y];
    var result: RibbonVertex;
    result.position = vec4(-2.0, -2.0, 0.0, 1.0);
    result.first = vec4(0.0);
    result.last = vec4(0.0);
    result.depths = vec4(0.0);
    result.interval = 0u;
    result.fast_mode = 0u;
    result.ink = vec4(0.0);
    result.support_a = vec4(0.0);
    result.support_b = vec4(0.0);
    if min(min(min(a0.w, b0.w), min(am.w, bm.w)), min(a1.w, b1.w)) < 0.5 {
        return result;
    }
    let width = max(params.style.x, 0.05);
    let half = max(width * 0.5, 0.5);
    var direction = bm.xy - am.xy;
    if dot(direction, direction) < 1e-10 { direction = b0.xy - a0.xy; }
    if dot(direction, direction) < 1e-10 { direction = vec2(1.0, 0.0); }
    direction = normalize(direction);
    let normal = vec2(-direction.y, direction.x);
    let old_offset = vec2(dot(a0.xy - am.xy, normal), dot(b0.xy - bm.xy, normal));
    let new_offset = vec2(dot(a1.xy - am.xy, normal), dot(b1.xy - bm.xy, normal));
    let old_clear = min(abs(old_offset.x), abs(old_offset.y)) > half
        && old_offset.x * old_offset.y > 0.0;
    let new_clear = min(abs(new_offset.x), abs(new_offset.y)) > half
        && new_offset.x * new_offset.y > 0.0;
    let old_side = select(-1.0, 1.0, old_offset.x + old_offset.y > 0.0);
    let new_side = select(-1.0, 1.0, new_offset.x + new_offset.y > 0.0);
    let old_a = am.xy + normal * half * old_side;
    let old_b = bm.xy + normal * half * old_side;
    let new_a = am.xy + normal * half * new_side;
    let new_b = bm.xy + normal * half * new_side;
    let old_cross = (old_offset.x * old_offset.y < 0.0 && max(abs(old_offset.x), abs(old_offset.y)) > half)
        || (old_clear && folded_quad(a0.xy, b0.xy, old_b, old_a));
    let new_cross = (new_offset.x * new_offset.y < 0.0 && max(abs(new_offset.x), abs(new_offset.y)) > half)
        || (new_clear && folded_quad(new_a, new_b, b1.xy, a1.xy));
    let travel_a = length(am.xy - a0.xy) + length(a1.xy - am.xy);
    let travel_b = length(bm.xy - b0.xy) + length(b1.xy - bm.xy);
    // A zero-area mathematical sweep can still move its finite caps a long
    // distance along the wire. Use time-clipped coverage for that case rather
    // than displaying only the stationary-looking midpoint body.
    let parallel_motion = max(max(abs(old_offset.x), abs(old_offset.y)),
        max(abs(new_offset.x), abs(new_offset.y))) <= half
        && max(travel_a, travel_b) > width;
    if old_cross || new_cross || parallel_motion {
        if corner < 12u { return interval_vertex(corner % 6u, edge_index * 2u + corner / 6u); }
        return result;
    }
    let remaining = width / (width + max(travel_a, travel_b));
    var retention = sqrt(remaining);
    if abs(params.style.z - 0.5) > 1e-6 { retention = pow(remaining, params.style.z); }
    let peak = params.style.y * (params.style.w + (1.0 - params.style.w) * retention);
    // Reversal wings can occupy the same spatial region. Split their shared
    // peak instead of accidentally doubling the central body's brightness.
    let wing_weight = select(1.0, 0.5, old_clear && new_clear && old_side == new_side);
    let section = corner / 6u;
    let quad_corner = corner % 6u;
    var points: array<vec2<f32>, 4>;
    var alphas: array<f32, 4>;
    var first_free = false;
    var last_free = false;
    if section == 0u {
        if !old_clear { return result; }
        points = array<vec2<f32>, 4>(a0.xy, b0.xy, old_b, old_a);
        alphas = array<f32, 4>(0.0, 0.0, peak * bm.z * wing_weight, peak * am.z * wing_weight);
    } else if section == 1u {
        points = array<vec2<f32>, 4>(am.xy - normal * half, bm.xy - normal * half,
            bm.xy + normal * half, am.xy + normal * half);
        alphas = array<f32, 4>(peak * am.z, peak * bm.z, peak * bm.z, peak * am.z);
        first_free = !(old_clear && old_side < 0.0) && !(new_clear && new_side < 0.0);
        last_free = !(old_clear && old_side > 0.0) && !(new_clear && new_side > 0.0);
    } else {
        if !new_clear { return result; }
        points = array<vec2<f32>, 4>(new_a, new_b, b1.xy, a1.xy);
        alphas = array<f32, 4>(peak * am.z * wing_weight, peak * bm.z * wing_weight, 0.0, 0.0);
    }
    let indices = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u);
    let index = indices[quad_corner];
    let point = points[index];
    result.position = vec4(point.x / params.viewport.x * 2.0 - 1.0,
        1.0 - point.y / params.viewport.y * 2.0, 0.0, 1.0);
    result.ink = vec4(params.tint.rgb, alphas[index]);
    if quad_corner < 3u {
        result.support_a = support_edge(points[1], points[2], true);
        result.support_b = support_edge(points[0], points[1], first_free);
    } else {
        result.support_a = support_edge(points[3], points[0], true);
        result.support_b = support_edge(points[2], points[3], last_free);
    }
    return result;
}

// Integral of the SINGLE shutter tent, whose vertices are [-2:0,-1:1,0:0].
// The two intervals each integrate to 0.5 at rest. Splitting never introduces
// two full-strength bodies or a double-strength shared temporal boundary.
fn profile_integral(low: f32, high: f32, interval: u32) -> f32 {
    let moment = (high * high - low * low) * 0.5;
    return select(moment, high - low - moment, interval == 1u);
}

fn clip_positive(range: vec2<f32>, offset: f32, velocity: f32) -> vec2<f32> {
    if abs(velocity) < 1e-6 {
        return select(vec2(1.0, 0.0), range, offset >= 0.0);
    }
    let crossing = -offset / velocity;
    return select(vec2(range.x, min(range.y, crossing)),
        vec2(max(range.x, crossing), range.y), velocity > 0.0);
}

fn stationary_ink(v: RibbonVertex, point: vec2<f32>, along: vec2<f32>) -> f32 {
    let a0 = v.first.xy;
    let b0 = v.first.zw;
    let a1 = v.last.xy;
    let b1 = v.last.zw;
    let relative = point - a0;
    let u = dot(relative, along);
    let across = vec2(-along.y, along.x);
    let width = max(params.style.x, 0.05);
    let half = width * 0.5;
    // Linear cap-time clipping also preserves a wire moving along its length.
    var range = vec2(0.0, 1.0);
    range = clip_positive(range, u + half, -dot(a1 - a0, along));
    range = clip_positive(range, dot(b0 - a0, along) + half - u, dot(b1 - b0, along));
    let middle_a = (a0 + a1) * 0.5;
    let middle_b = (b0 + b1) * 0.5;
    let middle = middle_b - middle_a;
    let s = clamp(dot(point - middle_a, middle) / max(dot(middle, middle), 1e-8), 0.0, 1.0);
    let nearest = middle_a + middle * s;
    let coverage = clamp(half + 0.5 - abs(dot(point - nearest, across)), 0.0, 1.0);
    let low = clamp(range.x, 0.0, 1.0);
    let high = clamp(range.y, 0.0, 1.0);
    if high <= low { return 0.0; }
    let depth = mix(mix(v.depths.x, v.depths.y, s), mix(v.depths.z, v.depths.w, s), 0.5);
    return coverage * profile_integral(low, high, v.interval) * depth;
}

fn root_ink(v: RibbonVertex, point: vec2<f32>, time: f32) -> f32 {
    let a0 = v.first.xy;
    let b0 = v.first.zw;
    let a1 = v.last.xy;
    let b1 = v.last.zw;
    let t = clamp(time, 0.0, 1.0);
    let a = mix(a0, a1, t);
    let b = mix(b0, b1, t);
    let e = b - a;
    let edge_length = length(e);
    if edge_length < 1e-5 { return 0.0; }
    let s = dot(point - a, e) / (edge_length * edge_length);
    let along = clamp(s, 0.0, 1.0);
    let velocity = mix(a1 - a0, b1 - b0, along);
    let movement = abs(cross2(e / edge_length, velocity));
    let width = max(params.style.x, 0.05);
    let reach = (width * 0.5 + 0.5) / max(movement, 1e-5);
    let low = max(0.0, time - reach);
    let high = min(1.0, time + reach);
    if high <= low { return 0.0; }
    // Reject extrapolated endpoint/cap roots that are outside the real stroke.
    let nearest = a + e * along;
    var coverage = 1.0;
    if s < 0.0 || s > 1.0 || time < 0.0 || time > 1.0 {
        coverage = clamp(width * 0.5 + 0.5 - distance(point, nearest), 0.0, 1.0);
    }
    // Match the current artistic speed fade approximately, using transverse
    // movement. The profile integral supplies temporal normalization; the gain
    // retains luminous trails instead of imposing a physical 1/speed falloff.
    let remaining = width / (width + movement * 2.0);
    var retention = sqrt(remaining);
    if abs(params.style.z - 0.5) > 1e-6 { retention = pow(remaining, params.style.z); }
    let fade = params.style.w + (1.0 - params.style.w) * retention;
    let gain = fade / min(1.0, reach * 2.0);
    let depth = mix(mix(v.depths.x, v.depths.y, along), mix(v.depths.z, v.depths.w, along), t);
    return coverage * profile_integral(low, high, v.interval) * gain * depth;
}

fn ribbon_alpha(v: RibbonVertex) -> f32 {
    let point = v.position.xy;
    let a = v.first.xy;
    let e = v.first.zw - a;
    let movement = v.last.xy - a;
    let delta_e = (v.last.zw - v.last.xy) - e;
    let middle_e = e + delta_e * 0.5;
    var along = middle_e;
    if length(along) < 1e-5 { along = e; }
    if length(along) < 1e-5 { along = vec2(1.0, 0.0); }
    along = normalize(along);
    let across_movement = max(abs(cross2(along, movement)),
        abs(cross2(along, movement + delta_e)));
    if across_movement < max(params.style.x * 0.25, 0.1) && dot(e, e + delta_e) >= 0.0 {
        return stationary_ink(v, point, along) * params.style.y;
    }
    // Invert the bilinear patch P(s,t)=A+s*E+t*V+s*t*dE. A rotating
    // sweep can have two roots; both are distinct shutter-time contributions.
    let relative = point - a;
    let c = cross2(relative, e);
    let b = cross2(relative, delta_e) - cross2(movement, e);
    let quadratic = -cross2(movement, delta_e);
    var ink = 0.0;
    if abs(quadratic) < 1e-5 {
        if abs(b) > 1e-5 { ink = root_ink(v, point, -c / b); }
    } else {
        let discriminant = b * b - 4.0 * quadratic * c;
        if discriminant >= 0.0 {
            let root = sqrt(discriminant);
            let t0 = (-b - root) / (2.0 * quadratic);
            let t1 = (-b + root) / (2.0 * quadratic);
            ink = root_ink(v, point, t0);
            if abs(t1 - t0) > 1e-4 { ink += root_ink(v, point, t1); }
        }
    }
    return ink * params.style.y;
}

@fragment
fn ribbon_additive(v: RibbonVertex) -> @location(0) vec4<f32> {
    let alpha = ribbon_alpha(v);
    return vec4(max(params.tint.rgb, vec3(1.0 / 255.0)) * alpha, alpha);
}

@fragment
fn ribbon_multiply(v: RibbonVertex) -> @location(0) vec4<f32> {
    let alpha = clamp(ribbon_alpha(v), 0.0, 1.0);
    return vec4(mix(vec3(1.0), min(params.tint.rgb, vec3(254.0 / 255.0)), alpha), 1.0);
}

fn fast_alpha(v: RibbonVertex) -> f32 {
    if v.fast_mode != 0u { return ribbon_alpha(v); }
    var nearest = 1e6;
    if v.support_a.w > 0.5 { nearest = min(nearest, abs(dot(v.support_a.xy, v.position.xy) + v.support_a.z)); }
    if v.support_b.w > 0.5 { nearest = min(nearest, abs(dot(v.support_b.xy, v.position.xy) + v.support_b.z)); }
    return v.ink.a * clamp(0.5 + nearest, 0.0, 1.0);
}

@fragment
fn ribbon_fast_additive(v: RibbonVertex) -> @location(0) vec4<f32> {
    let alpha = fast_alpha(v);
    return vec4(max(params.tint.rgb, vec3(1.0 / 255.0)) * alpha, alpha);
}

@fragment
fn ribbon_fast_multiply(v: RibbonVertex) -> @location(0) vec4<f32> {
    let alpha = clamp(fast_alpha(v), 0.0, 1.0);
    return vec4(mix(vec3(1.0), min(params.tint.rgb, vec3(254.0 / 255.0)), alpha), 1.0);
}
